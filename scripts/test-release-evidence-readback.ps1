[CmdletBinding()]
param(
  [string]$RepositoryRoot = (Split-Path -Parent $PSScriptRoot),
  [string]$EvaExecutable
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Utf8NoBom = New-Object System.Text.UTF8Encoding($false, $true)
$SourceCommit = "0123456789abcdef0123456789abcdef01234567"
$RunId = "123456789"
$RunAttempt = "1"
$ProducerJob = "release_evidence"
$UploadArtifactId = "987654321"
$UploadDigest = "$('a' * 64)"

function Assert-Equal {
  param(
    [object]$Expected,
    [object]$Actual,
    [string]$Message
  )

  if ([string]$Expected -cne [string]$Actual) {
    throw "$Message Expected '$Expected' but got '$Actual'."
  }
}

function Assert-Contains {
  param(
    [string]$Text,
    [string]$Expected,
    [string]$Message
  )

  if (-not $Text.Contains($Expected)) {
    throw "$Message Missing '$Expected'."
  }
}

function Assert-NotContains {
  param(
    [string]$Text,
    [string]$Unexpected,
    [string]$Message
  )

  if ($Text.Contains($Unexpected)) {
    throw "$Message Unexpected '$Unexpected'."
  }
}

function Write-Utf8LfFile {
  param(
    [string]$Path,
    [string]$Text
  )

  $parent = [System.IO.Path]::GetDirectoryName($Path)
  if (-not [string]::IsNullOrWhiteSpace($parent)) {
    [System.IO.Directory]::CreateDirectory($parent) | Out-Null
  }
  $normalized = $Text.Replace("`r`n", "`n").Replace("`r", "`n").TrimStart([char]0xFEFF)
  $normalized = $normalized.TrimEnd([char[]]@("`n")) + "`n"
  [System.IO.File]::WriteAllText($Path, $normalized, $Utf8NoBom)
}

function Copy-Bundle {
  param(
    [string]$Source,
    [string]$Destination
  )

  if ([System.IO.Directory]::Exists($Destination)) {
    Remove-Item -LiteralPath $Destination -Recurse -Force
  }
  Copy-Item -LiteralPath $Source -Destination $Destination -Recurse
}

function Rebind-McpCaptureSubject {
  param([string]$EvidenceRoot)

  $capturePath = Join-Path $EvidenceRoot "mcp-compatibility.capture.json"
  $capture = [System.IO.File]::ReadAllText($capturePath, $Utf8NoBom) | ConvertFrom-Json
  $argv = @($capture.argv)
  $subjectIndex = [System.Array]::IndexOf($argv, "--subject-output") + 1
  if ($subjectIndex -le 0 -or $subjectIndex -ge $argv.Count) {
    throw "MCP compatibility capture fixture has no subject output argument."
  }
  $capture.argv[$subjectIndex] = Join-Path $EvidenceRoot "release-mcp-compatibility.evidence"
  Write-Utf8LfFile $capturePath ($capture | ConvertTo-Json -Depth 8)
}

$repository = [System.IO.Path]::GetFullPath($RepositoryRoot)
$captureScript = Join-Path $repository "scripts/capture-release-evidence.ps1"
$prepareScript = Join-Path $repository "scripts/prepare-release-evidence-upload.ps1"
$verifyScript = Join-Path $repository "scripts/verify-release-evidence-readback.ps1"
foreach ($script in @($captureScript, $prepareScript, $verifyScript)) {
  if (-not [System.IO.File]::Exists($script)) {
    throw "Required release evidence script is missing: $script"
  }
}

if ([string]::IsNullOrWhiteSpace($EvaExecutable)) {
  $binaryName = if ($env:OS -eq "Windows_NT") { "eva.exe" } else { "eva" }
  $EvaExecutable = Join-Path $repository "target/debug/$binaryName"
}
$eva = [System.IO.Path]::GetFullPath($EvaExecutable)
if (-not [System.IO.File]::Exists($eva)) {
  throw "Eva test binary is missing at '$eva'. Run 'cargo build --locked --bin eva' first."
}

$cargoText = [System.IO.File]::ReadAllText((Join-Path $repository "Cargo.toml"), $Utf8NoBom)
$versionMatch = [regex]::Match($cargoText, '(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?)"\s*$')
if (-not $versionMatch.Success) {
  throw "Cannot resolve the Eva package version from Cargo.toml."
}
$version = $versionMatch.Groups[1].Value
$releaseTag = "v$version"
$uploadArtifactName = "release-evidence-$releaseTag-$RunId-$RunAttempt"
$timestamp = [System.DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() - 1000
$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) "eva-release-readback-$([guid]::NewGuid().ToString('N'))"
$sourceRoot = Join-Path $tempRoot "producer"
$sealedRoot = Join-Path $sourceRoot "release-evidence"
$artifactPath = Join-Path $sourceRoot "eva-cli-$version-x86_64-unknown-linux-gnu.tar.gz"
$metadataPath = Join-Path $sourceRoot "upload-seal.json"
$receiptsRoot = Join-Path $tempRoot "receipts"

try {
  [System.IO.Directory]::CreateDirectory($sealedRoot) | Out-Null
  [System.IO.Directory]::CreateDirectory($receiptsRoot) | Out-Null
  [System.IO.File]::WriteAllBytes($artifactPath, $Utf8NoBom.GetBytes("real release artifact bytes`n"))

  $distributionLines = New-Object System.Collections.Generic.List[string]
  $distributionLines.Add("format=eva.release.distribution_evidence.v1")
  $distributionLines.Add("version=$version")
  $distributionLines.Add("source_tag=$releaseTag")
  $distributionLines.Add("source_commit=$SourceCommit")
  $distributionLines.Add("docs.install=docs/en/release/install-upgrade-uninstall.md")
  $distributionLines.Add("docs.uninstall=docs/en/release/install-upgrade-uninstall.md")
  $distributionLines.Add("docs.upgrade=docs/en/release/install-upgrade-uninstall.md")
  $smokes = @(
    @("windows", "x86_64-pc-windows-msvc", "eva-cli-$version-x86_64-pc-windows-msvc.zip", "zip"),
    @("linux", "x86_64-unknown-linux-gnu", "eva-cli-$version-x86_64-unknown-linux-gnu.tar.gz", "tar.gz"),
    @("macos", "x86_64-apple-darwin", "eva-cli-$version-x86_64-apple-darwin.tar.gz", "tar.gz")
  )
  for ($index = 0; $index -lt $smokes.Count; $index += 1) {
    $smoke = $smokes[$index]
    $distributionLines.Add("smoke.$index.os=$($smoke[0])")
    $distributionLines.Add("smoke.$index.target=$($smoke[1])")
    $distributionLines.Add("smoke.$index.artifact=$($smoke[2])")
    $distributionLines.Add("smoke.$index.package_format=$($smoke[3])")
    $distributionLines.Add("smoke.$index.install_command=install $($smoke[2])")
    $distributionLines.Add("smoke.$index.smoke_command=eva --version")
    $distributionLines.Add("smoke.$index.uninstall_command=uninstall $($smoke[2])")
    $distributionLines.Add("smoke.$index.upgrade_command=upgrade $($smoke[2])")
    $distributionLines.Add("smoke.$index.status=passed")
  }
  $distributionLines.Add("package.0.manager=ghcr")
  $distributionLines.Add("package.0.package=ghcr.io/yetmos/eva-cli")
  $distributionLines.Add("package.0.target=linux/amd64+linux/arm64")
  $distributionLines.Add("package.0.command=docker buildx imagetools inspect ghcr.io/yetmos/eva-cli:$version")
  $distributionLines.Add("package.0.status=passed")
  Write-Utf8LfFile (Join-Path $sealedRoot "release-distribution.evidence") ($distributionLines -join "`n")

  Write-Utf8LfFile (Join-Path $sealedRoot "release-security-scan.evidence") (@(
      "format=eva.release.security_scan_evidence.v1"
      "version=$version"
      "source_tag=$releaseTag"
      "source_commit=$SourceCommit"
      "scanner=cargo-audit"
      "scanner_version=1.0.0"
      "scan_status=passed"
      "command=cargo audit --json"
    ) -join "`n")

  Write-Utf8LfFile (Join-Path $sealedRoot "release-benchmark.evidence") (@(
      "format=eva.release.benchmark_evidence.v1"
      "version=$version"
      "source_tag=$releaseTag"
      "source_commit=$SourceCommit"
      "benchmark_status=passed"
      "measurement.0.component=release.check"
      "measurement.0.metric=release check wall time"
      "measurement.0.budget_ms=5000"
      "measurement.0.observed_ms=120"
      "measurement.0.sample_count=3"
      "measurement.0.command=target/release/eva release check --output json"
      "measurement.0.environment=github-actions-ubuntu-latest"
    ) -join "`n")

  $mcpCompatibilityEvidence = Join-Path $sealedRoot "release-mcp-compatibility.evidence"
  $mcpCompatibilityCapture = Join-Path $sealedRoot "mcp-compatibility.capture.json"
  $runnerEnvironment = @{
    GITHUB_ACTIONS = $env:GITHUB_ACTIONS
    GITHUB_RUN_ID = $env:GITHUB_RUN_ID
    GITHUB_RUN_ATTEMPT = $env:GITHUB_RUN_ATTEMPT
    GITHUB_JOB = $env:GITHUB_JOB
    RUNNER_NAME = $env:RUNNER_NAME
    RUNNER_OS = $env:RUNNER_OS
    RUNNER_ARCH = $env:RUNNER_ARCH
  }
  try {
    $env:GITHUB_ACTIONS = "true"
    $env:GITHUB_RUN_ID = $RunId
    $env:GITHUB_RUN_ATTEMPT = $RunAttempt
    $env:GITHUB_JOB = $ProducerJob
    $env:RUNNER_NAME = "release-readback-contract"
    $env:RUNNER_OS = "Linux"
    $env:RUNNER_ARCH = "X64"
    & $captureScript `
      -Executable $eva `
      -ArgumentList @(
        "mcp", "compatibility", "measure",
        "--subject-output", $mcpCompatibilityEvidence,
        "--output", "json"
      ) `
      -ManifestPath $mcpCompatibilityCapture `
      -StdoutPath (Join-Path $sealedRoot "mcp-compatibility.stdout.json") `
      -StderrPath (Join-Path $sealedRoot "mcp-compatibility.stderr") `
      -CaptureId "mcp.compatibility.measure" | Out-Null
  } finally {
    foreach ($entry in $runnerEnvironment.GetEnumerator()) {
      [System.Environment]::SetEnvironmentVariable([string]$entry.Key, $entry.Value, "Process")
    }
  }
  if (-not [System.IO.File]::Exists($mcpCompatibilityEvidence)) {
    throw "MCP compatibility measurement did not write its canonical subject."
  }
  $mcpCompatibilityCaptureValue = [System.IO.File]::ReadAllText($mcpCompatibilityCapture, $Utf8NoBom) | ConvertFrom-Json
  $mcpCompatibilityTimestamp = [System.DateTimeOffset]::Parse(
    [string]$mcpCompatibilityCaptureValue.finished_at,
    [System.Globalization.CultureInfo]::InvariantCulture,
    [System.Globalization.DateTimeStyles]::RoundtripKind
  ).ToUnixTimeMilliseconds()

  Write-Utf8LfFile (Join-Path $sealedRoot "producer-note.txt") "producer payload included in the sealed file set"

  & $prepareScript `
    -EvidenceRoot $sealedRoot `
    -ArtifactPath $artifactPath `
    -ArtifactTarget "x86_64-unknown-linux-gnu" `
    -ArtifactFormat "tar.gz" `
    -ArtifactBinary "eva" `
    -ArtifactBuilder "github-actions:native-linux" `
    -ArtifactBuildCommand "cargo build --release --locked --bin eva" `
    -ArtifactSbom "unavailable:W9-L10" `
    -ArtifactScanStatus "passed" `
    -ReleaseTag $releaseTag `
    -SourceCommit $SourceCommit `
    -RunId $RunId `
    -RunAttempt $RunAttempt `
    -Job $ProducerJob `
    -ArtifactTimestampMilliseconds $timestamp `
    -DistributionTimestampMilliseconds $timestamp `
    -SecurityScanTimestampMilliseconds $timestamp `
    -BenchmarkTimestampMilliseconds $timestamp `
    -McpCompatibilityTimestampMilliseconds $mcpCompatibilityTimestamp `
    -UploadArtifactName $uploadArtifactName `
    -MetadataPath $metadataPath | Out-Null

  $metadata = [System.IO.File]::ReadAllText($metadataPath, $Utf8NoBom) | ConvertFrom-Json
  Assert-Equal "eva.release.evidence_upload_seal.v1" $metadata.schema "upload seal schema mismatch."
  Assert-Equal $uploadArtifactName $metadata.upload_artifact_name "upload artifact name mismatch."

  function Assert-UntrustedMcpPrepareFails {
    param(
      [string]$Name,
      [scriptblock]$Mutation,
      [string]$ExpectedReason
    )

    $caseProducer = Join-Path $tempRoot "producer-untrusted-$Name"
    $caseEvidence = Join-Path $caseProducer "release-evidence"
    [System.IO.Directory]::CreateDirectory($caseEvidence) | Out-Null
    foreach ($sourceName in @(
        "release-distribution.evidence",
        "release-security-scan.evidence",
        "release-benchmark.evidence",
        "release-mcp-compatibility.evidence",
        "mcp-compatibility.capture.json",
        "mcp-compatibility.stdout.json",
        "mcp-compatibility.stderr"
      )) {
      [System.IO.File]::Copy((Join-Path $sealedRoot $sourceName), (Join-Path $caseEvidence $sourceName), $false)
    }
    Rebind-McpCaptureSubject $caseEvidence
    & $Mutation $caseEvidence
    $failure = $null
    try {
      & $prepareScript `
        -EvidenceRoot $caseEvidence `
        -ArtifactPath $artifactPath `
        -ArtifactTarget "x86_64-unknown-linux-gnu" `
        -ArtifactFormat "tar.gz" `
        -ArtifactBinary "eva" `
        -ArtifactBuilder "github-actions:native-linux" `
        -ArtifactBuildCommand "cargo build --release --locked --bin eva" `
        -ArtifactSbom "unavailable:W9-L10" `
        -ArtifactScanStatus "passed" `
        -ReleaseTag $releaseTag `
        -SourceCommit $SourceCommit `
        -RunId $RunId `
        -RunAttempt $RunAttempt `
        -Job $ProducerJob `
        -ArtifactTimestampMilliseconds $timestamp `
        -DistributionTimestampMilliseconds $timestamp `
        -SecurityScanTimestampMilliseconds $timestamp `
        -BenchmarkTimestampMilliseconds $timestamp `
        -McpCompatibilityTimestampMilliseconds $mcpCompatibilityTimestamp `
        -UploadArtifactName "$uploadArtifactName-untrusted-$Name" `
        -MetadataPath (Join-Path $caseProducer "upload-seal.json") | Out-Null
    } catch {
      $failure = $_.Exception.Message
    }
    if ([string]::IsNullOrWhiteSpace($failure) -or -not $failure.Contains("reason=$ExpectedReason")) {
      throw "Untrusted MCP fixture '$Name' was not rejected for '$ExpectedReason': $failure"
    }
  }

  Assert-UntrustedMcpPrepareFails "missing-capture" {
    param($root)
    Remove-Item -LiteralPath (Join-Path $root "mcp-compatibility.capture.json") -Force
  } "upload_mcp_capture_invalid"
  Assert-UntrustedMcpPrepareFails "tampered-subject" {
    param($root)
    $path = Join-Path $root "release-mcp-compatibility.evidence"
    $text = [System.IO.File]::ReadAllText($path, $Utf8NoBom)
    Write-Utf8LfFile $path ($text.Replace("evidence_kind=measurement", "evidence_kind=fixture"))
  } "upload_mcp_capture_receipt_invalid"

  $reservedProducer = Join-Path $tempRoot "producer-reserved-name"
  $reservedEvidence = Join-Path $reservedProducer "release-evidence"
  [System.IO.Directory]::CreateDirectory($reservedEvidence) | Out-Null
  foreach ($sourceName in @(
      "release-distribution.evidence",
      "release-security-scan.evidence",
      "release-benchmark.evidence",
      "release-mcp-compatibility.evidence",
      "mcp-compatibility.capture.json",
      "mcp-compatibility.stdout.json",
      "mcp-compatibility.stderr"
    )) {
    [System.IO.File]::Copy((Join-Path $sealedRoot $sourceName), (Join-Path $reservedEvidence $sourceName), $false)
  }
  Rebind-McpCaptureSubject $reservedEvidence
  $reservedArtifact = Join-Path $reservedProducer "release-artifact.evidence"
  [System.IO.File]::WriteAllBytes($reservedArtifact, $Utf8NoBom.GetBytes("reserved artifact name`n"))
  $reservedFailure = $null
  try {
    & $prepareScript `
      -EvidenceRoot $reservedEvidence `
      -ArtifactPath $reservedArtifact `
      -ArtifactTarget "x86_64-unknown-linux-gnu" `
      -ArtifactFormat "tar.gz" `
      -ArtifactBinary "eva" `
      -ArtifactBuilder "github-actions:native-linux" `
      -ArtifactBuildCommand "cargo build --release --locked --bin eva" `
      -ArtifactSbom "unavailable:W9-L10" `
      -ArtifactScanStatus "passed" `
      -ReleaseTag $releaseTag `
      -SourceCommit $SourceCommit `
      -RunId $RunId `
      -RunAttempt $RunAttempt `
      -Job $ProducerJob `
      -ArtifactTimestampMilliseconds $timestamp `
      -DistributionTimestampMilliseconds $timestamp `
      -SecurityScanTimestampMilliseconds $timestamp `
      -BenchmarkTimestampMilliseconds $timestamp `
      -McpCompatibilityTimestampMilliseconds $mcpCompatibilityTimestamp `
      -UploadArtifactName "$uploadArtifactName-reserved" `
      -MetadataPath (Join-Path $reservedProducer "upload-seal.json") | Out-Null
  } catch {
    $reservedFailure = $_.Exception.Message
  }
  if ([string]::IsNullOrWhiteSpace($reservedFailure) -or -not $reservedFailure.Contains("reason=upload_artifact_name_conflict")) {
    throw "Reserved artifact basename was not rejected: $reservedFailure"
  }

  $common = @{
    ExpectedReleaseTag = $releaseTag
    ExpectedSourceCommit = $SourceCommit
    ExpectedRunId = $RunId
    ExpectedRunAttempt = $RunAttempt
    ExpectedProducerJob = $ProducerJob
    ExpectedUploadArtifactName = $uploadArtifactName
    ExpectedUploadArtifactId = $UploadArtifactId
    ExpectedUploadDigest = $UploadDigest
    ExpectedIndexDigest = [string]$metadata.index_digest
    ExpectedManifestDigest = [string]$metadata.manifest_digest
    ExpectedBundleDigest = [string]$metadata.bundle_digest
    ExpectedFileCount = [string]$metadata.file_count
    EvaExecutable = $eva
  }

  $successRoot = Join-Path $tempRoot "download-success"
  Copy-Bundle $sealedRoot $successRoot
  $successReceipt = Join-Path $receiptsRoot "success.json"
  Push-Location $repository
  try {
    $success = & $verifyScript @common -EvidenceRoot $successRoot -ReceiptPath $successReceipt
  } finally {
    Pop-Location
  }
  Assert-Equal "blocked" $success.DecisionStatus "verified production report status mismatch."
  Assert-Equal 3 $success.ProcessExitCode "verified production report exit mismatch."
  $receipt = [System.IO.File]::ReadAllText($successReceipt, $Utf8NoBom) | ConvertFrom-Json
  Assert-Equal "verified" $receipt.status "readback receipt status mismatch."
  Assert-Equal $metadata.index_digest $receipt.readback.index_digest "receipt index digest mismatch."
  Assert-Equal $metadata.manifest_digest $receipt.readback.manifest_digest "receipt manifest digest mismatch."
  Assert-Equal $metadata.bundle_digest $receipt.readback.bundle_digest "receipt bundle digest mismatch."
  Assert-Equal $UploadDigest $receipt.upload_artifact.digest "receipt upload digest mismatch."

  function Assert-ReadbackFails {
    param(
      [string]$Name,
      [scriptblock]$Mutation,
      [string]$ExpectedReason,
      [hashtable]$Overrides = @{}
    )

    $caseRoot = Join-Path $tempRoot "download-$Name"
    Copy-Bundle $sealedRoot $caseRoot
    & $Mutation $caseRoot
    $arguments = @{}
    foreach ($key in $common.Keys) {
      $arguments[$key] = $common[$key]
    }
    foreach ($key in $Overrides.Keys) {
      $arguments[$key] = $Overrides[$key]
    }
    $caseReceipt = Join-Path $receiptsRoot "$Name.json"
    $message = $null
    Push-Location $repository
    try {
      try {
        & $verifyScript @arguments -EvidenceRoot $caseRoot -ReceiptPath $caseReceipt | Out-Null
      } catch {
        $message = $_.Exception.Message
      }
    } finally {
      Pop-Location
    }
    if ([string]::IsNullOrWhiteSpace($message)) {
      throw "Readback case '$Name' unexpectedly succeeded."
    }
    if (-not $message.Contains("reason=$ExpectedReason")) {
      throw "Readback case '$Name' failed for the wrong reason: $message"
    }
    if ([System.IO.File]::Exists($caseReceipt)) {
      throw "Readback case '$Name' wrote a receipt after failure."
    }
  }

  Assert-ReadbackFails "deleted" {
    param($root)
    Remove-Item -LiteralPath (Join-Path $root "production/release-benchmark.evidence") -Force
  } "readback_file_set_mismatch"

  Assert-ReadbackFails "tampered" {
    param($root)
    $path = Join-Path $root "production/release-security-scan.evidence"
    $bytes = [System.IO.File]::ReadAllBytes($path)
    $bytes[0] = [byte]($bytes[0] -bxor 1)
    [System.IO.File]::WriteAllBytes($path, $bytes)
  } "readback_file_digest_mismatch"

  Assert-ReadbackFails "extra" {
    param($root)
    Write-Utf8LfFile (Join-Path $root "unindexed.txt") "not present in the producer index"
  } "readback_file_set_mismatch"

  Assert-ReadbackFails "wrong-index-digest" { param($root) } "readback_index_digest_mismatch" @{
    ExpectedIndexDigest = "sha256:$('b' * 64)"
  }

  Assert-ReadbackFails "wrong-manifest-digest" { param($root) } "readback_index_identity_mismatch" @{
    ExpectedManifestDigest = "sha256:$('c' * 64)"
  }

  Assert-ReadbackFails "wrong-bundle-digest" { param($root) } "readback_index_identity_mismatch" @{
    ExpectedBundleDigest = "sha256:$('d' * 64)"
  }

  Assert-ReadbackFails "wrong-file-count" { param($root) } "readback_file_count_mismatch" @{
    ExpectedFileCount = [string]([int]$metadata.file_count + 1)
  }

  function Assert-HonestGateFailure {
    param(
      [string]$Name,
      [string]$EvidenceName,
      [string]$OriginalValue,
      [string]$ReplacementValue,
      [string]$ArtifactScanStatus = "passed",
      [string]$ExpectedReason = "readback_gate_status_invalid"
    )

    $caseProducer = Join-Path $tempRoot "producer-$Name"
    $caseEvidence = Join-Path $caseProducer "release-evidence"
    [System.IO.Directory]::CreateDirectory($caseEvidence) | Out-Null
    foreach ($sourceName in @(
        "release-distribution.evidence",
        "release-security-scan.evidence",
        "release-benchmark.evidence",
        "release-mcp-compatibility.evidence",
        "mcp-compatibility.capture.json",
        "mcp-compatibility.stdout.json",
        "mcp-compatibility.stderr",
        "producer-note.txt"
      )) {
      [System.IO.File]::Copy((Join-Path $sealedRoot $sourceName), (Join-Path $caseEvidence $sourceName), $false)
    }
    Rebind-McpCaptureSubject $caseEvidence
    if (-not [string]::IsNullOrWhiteSpace($EvidenceName)) {
      $failurePath = Join-Path $caseEvidence $EvidenceName
      $failureText = [System.IO.File]::ReadAllText($failurePath, $Utf8NoBom)
      if (-not $failureText.Contains($OriginalValue)) {
        throw "Gate failure fixture '$Name' cannot find '$OriginalValue'."
      }
      Write-Utf8LfFile $failurePath ($failureText.Replace($OriginalValue, $ReplacementValue))
    }

    $caseArtifactName = "$uploadArtifactName-$Name"
    $caseMetadataPath = Join-Path $caseProducer "upload-seal.json"
    & $prepareScript `
      -EvidenceRoot $caseEvidence `
      -ArtifactPath $artifactPath `
      -ArtifactTarget "x86_64-unknown-linux-gnu" `
      -ArtifactFormat "tar.gz" `
      -ArtifactBinary "eva" `
      -ArtifactBuilder "github-actions:native-linux" `
      -ArtifactBuildCommand "cargo build --release --locked --bin eva" `
      -ArtifactSbom "unavailable:W9-L10" `
      -ArtifactScanStatus $ArtifactScanStatus `
      -ReleaseTag $releaseTag `
      -SourceCommit $SourceCommit `
      -RunId $RunId `
      -RunAttempt $RunAttempt `
      -Job $ProducerJob `
      -ArtifactTimestampMilliseconds $timestamp `
      -DistributionTimestampMilliseconds $timestamp `
      -SecurityScanTimestampMilliseconds $timestamp `
      -BenchmarkTimestampMilliseconds $timestamp `
      -McpCompatibilityTimestampMilliseconds $mcpCompatibilityTimestamp `
      -UploadArtifactName $caseArtifactName `
      -MetadataPath $caseMetadataPath | Out-Null
    $caseMetadata = [System.IO.File]::ReadAllText($caseMetadataPath, $Utf8NoBom) | ConvertFrom-Json
    $caseDownload = Join-Path $tempRoot "download-$Name"
    Copy-Bundle $caseEvidence $caseDownload
    $arguments = @{}
    foreach ($key in $common.Keys) {
      $arguments[$key] = $common[$key]
    }
    $arguments.ExpectedUploadArtifactName = $caseArtifactName
    $arguments.ExpectedIndexDigest = [string]$caseMetadata.index_digest
    $arguments.ExpectedManifestDigest = [string]$caseMetadata.manifest_digest
    $arguments.ExpectedBundleDigest = [string]$caseMetadata.bundle_digest
    $arguments.ExpectedFileCount = [string]$caseMetadata.file_count
    $caseReceipt = Join-Path $receiptsRoot "$Name.json"
    $message = $null
    Push-Location $repository
    try {
      try {
        & $verifyScript @arguments -EvidenceRoot $caseDownload -ReceiptPath $caseReceipt | Out-Null
      } catch {
        $message = $_.Exception.Message
      }
    } finally {
      Pop-Location
    }
    if ([string]::IsNullOrWhiteSpace($message) -or -not $message.Contains("reason=$ExpectedReason")) {
      throw "Gate failure fixture '$Name' did not fail at the external gate status: $message"
    }
    if ([System.IO.File]::Exists($caseReceipt)) {
      throw "Gate failure fixture '$Name' wrote a verified receipt."
    }
  }

  Assert-HonestGateFailure "failed-security" "release-security-scan.evidence" "scan_status=passed" "scan_status=failed"
  Assert-HonestGateFailure "failed-benchmark" "release-benchmark.evidence" "benchmark_status=passed" "benchmark_status=failed"
  Assert-HonestGateFailure `
    -Name "blocked-artifact-scan" `
    -EvidenceName "" `
    -OriginalValue "" `
    -ReplacementValue "" `
    -ArtifactScanStatus "blocked" `
    -ExpectedReason "readback_artifact_blocker_unexpected"

  $ciWorkflow = [System.IO.File]::ReadAllText((Join-Path $repository ".github/workflows/ci.yml"), $Utf8NoBom)
  $releaseWorkflow = [System.IO.File]::ReadAllText((Join-Path $repository ".github/workflows/release.yml"), $Utf8NoBom)
  Assert-Contains $ciWorkflow "./scripts/test-release-evidence-readback.ps1" "CI readback contract wiring mismatch."
  Assert-Contains $ciWorkflow "./scripts/test-mcp-compatibility-evidence.ps1" "CI MCP evidence contract wiring mismatch."
  Assert-Contains $ciWorkflow "./scripts/validate-mcp-compatibility-evidence.ps1" "CI MCP three-platform readback wiring mismatch."
  Assert-Contains $ciWorkflow "w4-mcp-compatibility-verified-" "CI MCP verified bundle upload wiring mismatch."
  Assert-Contains $ciWorkflow "shell: powershell" "CI PowerShell 5.1 contract wiring mismatch."
  Assert-Contains $releaseWorkflow "  release_evidence:" "Release evidence producer job wiring mismatch."
  Assert-Contains $releaseWorkflow "needs: release_evidence" "Release evidence consumer dependency mismatch."
  Assert-Contains $releaseWorkflow "artifact-ids: `${{ needs.release_evidence.outputs.artifact_id }}" "Immutable artifact ID download wiring mismatch."
  Assert-Contains $releaseWorkflow "include-hidden-files: true" "Hidden file upload parity mismatch."
  Assert-Contains $releaseWorkflow "-ExpectedManifestDigest `$env:EXPECTED_MANIFEST_DIGEST" "External manifest digest wiring mismatch."
  Assert-Contains $releaseWorkflow "Artifact ID download did not produce exactly the trusted artifact directory." "Downloaded artifact directory isolation mismatch."
  Assert-Contains $releaseWorkflow "./scripts/verify-release-evidence-readback.ps1" "Readback verifier workflow wiring mismatch."
  Assert-Contains $releaseWorkflow '"mcp", "compatibility", "measure"' "Release MCP measurement command wiring mismatch."
  Assert-Contains $releaseWorkflow '"release-evidence/release-mcp-compatibility.evidence"' "Release MCP subject wiring mismatch."
  Assert-NotContains $releaseWorkflow "Verify release evidence gates" "Legacy in-job release gate must be removed."
  Assert-NotContains $releaseWorkflow "release.verify-check" "Legacy in-job release gate capture must be removed."
  Assert-Equal 1 ([regex]::Matches($releaseWorkflow, '(?m)^\s+contents: write\s*$').Count) "Only the final publish job may write repository contents."

  Write-Host "Release evidence readback contract passed: verified, transport tamper, external digest mismatch, and failed measurement cases."
} finally {
  if ([System.IO.Directory]::Exists($tempRoot)) {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force
  }
}
