[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$EvidenceRoot,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ArtifactPath,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ArtifactTarget,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ArtifactFormat,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ArtifactBinary,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ArtifactBuilder,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ArtifactBuildCommand,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ArtifactSbom,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ArtifactScanStatus,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ReleaseTag,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$SourceCommit,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$RunId,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$RunAttempt,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$Job,

  [Parameter(Mandatory = $true)]
  [long]$ArtifactTimestampMilliseconds,

  [Parameter(Mandatory = $true)]
  [long]$DistributionTimestampMilliseconds,

  [Parameter(Mandatory = $true)]
  [long]$SecurityScanTimestampMilliseconds,

  [Parameter(Mandatory = $true)]
  [long]$BenchmarkTimestampMilliseconds,

  [Parameter(Mandatory = $true)]
  [long]$McpCompatibilityTimestampMilliseconds,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$UploadArtifactName,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$MetadataPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$EnvelopeFormat = "eva.release.evidence_envelope.v1"
$ManifestFormat = "eva.release.evidence_manifest.v1"
$IndexFormat = "eva.release.evidence_readback_index.v1"
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false, $true)

function Fail-UploadSeal {
  param(
    [string]$Reason,
    [string]$Detail
  )

  $safeDetail = if ([string]::IsNullOrWhiteSpace($Detail)) {
    "none"
  } else {
    $Detail.Replace("`r", " ").Replace("`n", " ")
  }
  throw "[release-evidence-upload] reason=$Reason detail=$safeDetail"
}

function Get-FullPath {
  param([string]$Path)

  try {
    if ([System.IO.Path]::IsPathRooted($Path)) {
      return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $Path))
  } catch {
    Fail-UploadSeal "upload_path_invalid" $Path
  }
}

function Get-PathComparison {
  if ($env:OS -eq "Windows_NT") {
    return [System.StringComparison]::OrdinalIgnoreCase
  }
  return [System.StringComparison]::Ordinal
}

function Assert-SingleLineValue {
  param(
    [string]$Value,
    [string]$Field,
    [int]$MaximumLength = 512
  )

  if ([string]::IsNullOrWhiteSpace($Value) -or $Value.Length -gt $MaximumLength -or
      $Value.Contains("`r") -or $Value.Contains("`n") -or $Value.Contains([char]0)) {
    Fail-UploadSeal "upload_field_invalid" $Field
  }
}

function Assert-TokenValue {
  param(
    [string]$Value,
    [string]$Field,
    [int]$MaximumLength = 256
  )

  Assert-SingleLineValue $Value $Field $MaximumLength
  if ($Value.Trim() -cne $Value -or $Value -match '\s') {
    Fail-UploadSeal "upload_field_invalid" $Field
  }
}

function Assert-StableFileName {
  param(
    [string]$Value,
    [string]$Field
  )

  Assert-TokenValue $Value $Field 255
  if ($Value.Contains('/') -or $Value.Contains('\') -or $Value.Contains('..')) {
    Fail-UploadSeal "upload_field_invalid" $Field
  }
}

function Test-IsChildPath {
  param(
    [string]$Root,
    [string]$Path
  )

  $rootPrefix = $Root.TrimEnd([char[]]@('/', '\')) + [System.IO.Path]::DirectorySeparatorChar
  return $Path.StartsWith($rootPrefix, (Get-PathComparison))
}

function Assert-RegularFile {
  param(
    [string]$Path,
    [string]$Reason
  )

  if (-not [System.IO.File]::Exists($Path)) {
    Fail-UploadSeal $Reason $Path
  }
  $attributes = [System.IO.File]::GetAttributes($Path)
  if (($attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
    Fail-UploadSeal "upload_path_symlink" $Path
  }
}

function Get-Sha256 {
  param([byte[]]$Bytes)

  $sha256 = [System.Security.Cryptography.SHA256]::Create()
  try {
    $digest = $sha256.ComputeHash($Bytes)
    return "sha256:$([System.BitConverter]::ToString($digest).Replace('-', '').ToLowerInvariant())"
  } finally {
    $sha256.Dispose()
  }
}

function Read-StrictUtf8File {
  param(
    [string]$Path,
    [string]$Reason
  )

  Assert-RegularFile $Path $Reason
  try {
    return $Utf8NoBom.GetString([System.IO.File]::ReadAllBytes($Path))
  } catch {
    Fail-UploadSeal "upload_evidence_utf8_invalid" $Path
  }
}

function Assert-McpCaptureStream {
  param(
    $Claim,
    [string]$Root,
    [string]$ExpectedName,
    [string]$Field
  )

  if ($null -eq $Claim -or [string]$Claim.path -cne $ExpectedName -or
      [string]$Claim.sha256 -cnotmatch '^sha256:[0-9a-f]{64}$' -or
      [long]$Claim.byte_count -lt 0) {
    Fail-UploadSeal "upload_mcp_capture_stream_invalid" $Field
  }
  $path = Join-Path $Root $ExpectedName
  Assert-RegularFile $path "upload_mcp_capture_stream_missing"
  $bytes = [System.IO.File]::ReadAllBytes($path)
  if ($bytes.LongLength -ne [long]$Claim.byte_count -or (Get-Sha256 $bytes) -cne [string]$Claim.sha256) {
    Fail-UploadSeal "upload_mcp_capture_stream_digest_mismatch" $Field
  }
  return $path
}

function Assert-McpCompatibilityCapture {
  param(
    [string]$Root,
    [string]$SubjectPath,
    [string]$ExpectedRunId,
    [string]$ExpectedRunAttempt,
    [string]$ExpectedJob,
    [long]$ExpectedTimestampMilliseconds
  )

  $capturePath = Join-Path $Root "mcp-compatibility.capture.json"
  try {
    $capture = (Read-StrictUtf8File $capturePath "upload_mcp_capture_missing") | ConvertFrom-Json
  } catch {
    Fail-UploadSeal "upload_mcp_capture_invalid" $capturePath
  }
  if ([string]$capture.format -cne "eva.release.command_capture.v1" -or
      [string]$capture.capture_id -cne "mcp.compatibility.measure" -or
      [string]$capture.outcome -cne "success" -or [int]$capture.exit_code -ne 0 -or
      $null -ne $capture.failure_reason) {
    Fail-UploadSeal "upload_mcp_capture_outcome_invalid" $capturePath
  }

  $argv = @($capture.argv | ForEach-Object { [string]$_ })
  $commandStart = 0
  if ([string]$capture.executable -ceq "cargo") {
    if ($argv.Count -ne 10 -or $argv[0] -cne "run" -or $argv[1] -cne "--quiet" -or $argv[2] -cne "--") {
      Fail-UploadSeal "upload_mcp_capture_command_invalid" "cargo argv"
    }
    $commandStart = 3
  } elseif ([System.IO.Path]::GetFileNameWithoutExtension([string]$capture.executable) -ceq "eva") {
    if ($argv.Count -ne 7) {
      Fail-UploadSeal "upload_mcp_capture_command_invalid" "eva argv"
    }
  } else {
    Fail-UploadSeal "upload_mcp_capture_command_invalid" ([string]$capture.executable)
  }
  $expectedCommand = @("mcp", "compatibility", "measure", "--subject-output")
  for ($index = 0; $index -lt $expectedCommand.Count; $index += 1) {
    if ($argv[$commandStart + $index] -cne $expectedCommand[$index]) {
      Fail-UploadSeal "upload_mcp_capture_command_invalid" ([string]$index)
    }
  }
  $capturedSubject = Get-FullPath $argv[$commandStart + 4]
  if (-not $capturedSubject.Equals($SubjectPath, (Get-PathComparison)) -or
      $argv[$commandStart + 5] -cne "--output" -or $argv[$commandStart + 6] -cne "json") {
    Fail-UploadSeal "upload_mcp_capture_command_invalid" "subject/output"
  }

  $runner = $capture.runner
  if ($null -eq $runner -or [string]$runner.provider -cne "github-actions" -or
      [string]$runner.run_id -cne $ExpectedRunId -or
      [string]$runner.run_attempt -cne $ExpectedRunAttempt -or
      [string]$runner.job -cne $ExpectedJob) {
    Fail-UploadSeal "upload_mcp_capture_runner_invalid" "$ExpectedRunId/$ExpectedRunAttempt/$ExpectedJob"
  }
  try {
    $finishedAt = [System.DateTimeOffset]::Parse(
      [string]$capture.finished_at,
      [System.Globalization.CultureInfo]::InvariantCulture,
      [System.Globalization.DateTimeStyles]::RoundtripKind
    )
  } catch {
    Fail-UploadSeal "upload_mcp_capture_timestamp_invalid" ([string]$capture.finished_at)
  }
  if ($finishedAt.ToUnixTimeMilliseconds() -ne $ExpectedTimestampMilliseconds) {
    Fail-UploadSeal "upload_mcp_capture_timestamp_mismatch" ([string]$capture.finished_at)
  }

  $stdoutPath = Assert-McpCaptureStream $capture.stdout $Root "mcp-compatibility.stdout.json" "stdout"
  $stderrPath = Assert-McpCaptureStream $capture.stderr $Root "mcp-compatibility.stderr" "stderr"
  if ([System.IO.FileInfo]::new($stderrPath).Length -ne 0) {
    Fail-UploadSeal "upload_mcp_capture_stderr_nonempty" $stderrPath
  }
  try {
    $receipt = (Read-StrictUtf8File $stdoutPath "upload_mcp_capture_stdout_missing") | ConvertFrom-Json
  } catch {
    Fail-UploadSeal "upload_mcp_capture_stdout_invalid" $stdoutPath
  }
  $subjectDigest = Get-Sha256 ([System.IO.File]::ReadAllBytes($SubjectPath))
  if ($receipt.ok -ne $true -or [string]$receipt.command -cne "mcp.compatibility.measure" -or
      [int]$receipt.exit_code -ne 0 -or [string]$receipt.data.evidence_kind -cne "measurement" -or
      [string]$receipt.data.subject.sha256 -cne $subjectDigest -or
      $receipt.data.subject.written -ne $true) {
    Fail-UploadSeal "upload_mcp_capture_receipt_invalid" $stdoutPath
  }
}

function Write-Utf8LfFile {
  param(
    [string]$Path,
    [string]$Text
  )

  $normalized = $Text.Replace("`r`n", "`n").Replace("`r", "`n").TrimStart([char]0xFEFF)
  $normalized = $normalized.TrimEnd([char[]]@("`n")) + "`n"
  [System.IO.File]::WriteAllText($Path, $normalized, $Utf8NoBom)
}

function Copy-CanonicalManifest {
  param(
    [string]$Source,
    [string]$Destination
  )

  Assert-RegularFile $Source "upload_evidence_missing"
  $text = [System.IO.File]::ReadAllText($Source, $Utf8NoBom)
  Write-Utf8LfFile $Destination $text
}

function Read-KeyValueFields {
  param([string]$Path)

  $fields = @{}
  foreach ($rawLine in [System.IO.File]::ReadAllLines($Path, $Utf8NoBom)) {
    $line = $rawLine.TrimStart([char]0xFEFF).Trim()
    if ([string]::IsNullOrEmpty($line) -or $line.StartsWith("#")) {
      continue
    }
    $separator = $line.IndexOf('=')
    if ($separator -le 0) {
      Fail-UploadSeal "upload_evidence_manifest_invalid" $Path
    }
    $key = $line.Substring(0, $separator).Trim()
    if ($fields.ContainsKey($key)) {
      Fail-UploadSeal "upload_evidence_field_duplicate" $key
    }
    $fields[$key] = $line.Substring($separator + 1).Trim()
  }
  return $fields
}

function Assert-EvidenceIdentity {
  param([string]$Path)

  Assert-RegularFile $Path "upload_evidence_missing"
  $fields = Read-KeyValueFields $Path
  $version = $ReleaseTag.Substring(1)
  foreach ($pair in @(
      @("version", $version),
      @("source_tag", $ReleaseTag),
      @("source_commit", $SourceCommit)
    )) {
    if (-not $fields.ContainsKey($pair[0]) -or [string]$fields[$pair[0]] -cne [string]$pair[1]) {
      Fail-UploadSeal "upload_evidence_identity_mismatch" "${Path}:$($pair[0])"
    }
  }
}

function Get-RelativePath {
  param(
    [string]$Root,
    [string]$Path
  )

  if (-not (Test-IsChildPath $Root $Path)) {
    Fail-UploadSeal "upload_path_escape" $Path
  }
  $prefix = $Root.TrimEnd([char[]]@('/', '\')) + [System.IO.Path]::DirectorySeparatorChar
  $relative = $Path.Substring($prefix.Length).Replace('\', '/')
  if ([string]::IsNullOrWhiteSpace($relative) -or $relative.Contains(':') -or
      @($relative.Split('/') | Where-Object { [string]::IsNullOrEmpty($_) -or $_ -eq "." -or $_ -eq ".." }).Count -gt 0) {
    Fail-UploadSeal "upload_relative_path_invalid" $relative
  }
  return $relative
}

if ($ReleaseTag -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?$') {
  Fail-UploadSeal "upload_release_tag_invalid" $ReleaseTag
}
if ($SourceCommit -cnotmatch '^[0-9a-f]{40}$') {
  Fail-UploadSeal "upload_source_commit_invalid" $SourceCommit
}
if ($RunId -notmatch '^[1-9][0-9]*$' -or $RunAttempt -notmatch '^[1-9][0-9]*$') {
  Fail-UploadSeal "upload_run_identity_invalid" "$RunId/$RunAttempt"
}
if ($Job -notmatch '^[0-9A-Za-z][0-9A-Za-z_.-]*$') {
  Fail-UploadSeal "upload_job_invalid" $Job
}
if ($UploadArtifactName -notmatch '^[0-9A-Za-z][0-9A-Za-z_.-]{0,127}$') {
  Fail-UploadSeal "upload_artifact_name_invalid" $UploadArtifactName
}
Assert-TokenValue $ArtifactTarget "artifact_target"
Assert-TokenValue $ArtifactFormat "artifact_format"
Assert-StableFileName $ArtifactBinary "artifact_binary"
foreach ($field in @(
    @("artifact_builder", $ArtifactBuilder),
    @("artifact_build_command", $ArtifactBuildCommand),
    @("artifact_sbom", $ArtifactSbom)
  )) {
  Assert-SingleLineValue ([string]$field[1]) ([string]$field[0]) 1024
  if ([string]$field[1] -cne ([string]$field[1]).Trim()) {
    Fail-UploadSeal "upload_field_invalid" ([string]$field[0])
  }
}
Assert-TokenValue $ArtifactScanStatus "artifact_scan_status"
foreach ($timestamp in @(
    $ArtifactTimestampMilliseconds,
    $DistributionTimestampMilliseconds,
    $SecurityScanTimestampMilliseconds,
    $BenchmarkTimestampMilliseconds,
    $McpCompatibilityTimestampMilliseconds
  )) {
  if ($timestamp -le 0) {
    Fail-UploadSeal "upload_timestamp_invalid" ([string]$timestamp)
  }
}

$root = Get-FullPath $EvidenceRoot
$artifact = Get-FullPath $ArtifactPath
$metadata = Get-FullPath $MetadataPath
if (-not [System.IO.Directory]::Exists($root)) {
  Fail-UploadSeal "upload_evidence_root_missing" $root
}
$rootAttributes = [System.IO.File]::GetAttributes($root)
if (($rootAttributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
  Fail-UploadSeal "upload_evidence_root_symlink" $root
}
Assert-RegularFile $artifact "upload_artifact_missing"
if (Test-IsChildPath $root $metadata) {
  Fail-UploadSeal "upload_metadata_inside_bundle" $metadata
}
if ($metadata.Equals($root, (Get-PathComparison))) {
  Fail-UploadSeal "upload_metadata_inside_bundle" $metadata
}

$productionRoot = Join-Path $root "production"
$indexPath = Join-Path $root "readback-index.manifest"
if ([System.IO.Directory]::Exists($productionRoot) -or [System.IO.File]::Exists($indexPath)) {
  Fail-UploadSeal "upload_bundle_already_prepared" $root
}
[System.IO.Directory]::CreateDirectory($productionRoot) | Out-Null

$distributionSource = Join-Path $root "release-distribution.evidence"
$securitySource = Join-Path $root "release-security-scan.evidence"
$benchmarkSource = Join-Path $root "release-benchmark.evidence"
$mcpCompatibilitySource = Join-Path $root "release-mcp-compatibility.evidence"
foreach ($source in @($distributionSource, $securitySource, $benchmarkSource)) {
  Assert-EvidenceIdentity $source
}
Assert-RegularFile $mcpCompatibilitySource "upload_evidence_missing"
Assert-McpCompatibilityCapture `
  -Root $root `
  -SubjectPath $mcpCompatibilitySource `
  -ExpectedRunId $RunId `
  -ExpectedRunAttempt $RunAttempt `
  -ExpectedJob $Job `
  -ExpectedTimestampMilliseconds $McpCompatibilityTimestampMilliseconds

$artifactName = [System.IO.Path]::GetFileName($artifact)
Assert-StableFileName $artifactName "artifact_name"
$reservedProductionNames = @(
  "release-artifact.evidence",
  "release-distribution.evidence",
  "release-security-scan.evidence",
  "release-benchmark.evidence",
  "release-mcp-compatibility.evidence",
  "release-artifact.envelope",
  "release-distribution.envelope",
  "release-security-scan.envelope",
  "release-benchmark.envelope",
  "release-mcp-compatibility.envelope",
  "release-evidence.manifest"
)
foreach ($reservedName in $reservedProductionNames) {
  if ($artifactName.Equals($reservedName, (Get-PathComparison))) {
    Fail-UploadSeal "upload_artifact_name_conflict" $artifactName
  }
}
$artifactSubjectPath = Join-Path $productionRoot $artifactName
[System.IO.File]::Copy($artifact, $artifactSubjectPath, $false)
$artifactBytes = [System.IO.File]::ReadAllBytes($artifactSubjectPath)
if ($artifactBytes.LongLength -eq 0) {
  Fail-UploadSeal "upload_artifact_empty" $artifactName
}
$artifactDigest = Get-Sha256 $artifactBytes
$version = $ReleaseTag.Substring(1)
$artifactEvidencePath = Join-Path $productionRoot "release-artifact.evidence"
$artifactEvidence = @(
  "format=eva.release.artifact_evidence.v1"
  "version=$version"
  "source_tag=$ReleaseTag"
  "source_commit=$SourceCommit"
  "artifact.name=$artifactName"
  "artifact.target=$ArtifactTarget"
  "artifact.format=$ArtifactFormat"
  "artifact.binary=$ArtifactBinary"
  "artifact.digest=$artifactDigest"
  "artifact.size_bytes=$($artifactBytes.LongLength)"
  "artifact.signed=false"
  "provenance.builder=$ArtifactBuilder"
  "provenance.source_commit=$SourceCommit"
  "provenance.build_command=$ArtifactBuildCommand"
  "provenance.build_profile=release"
  "provenance.sbom=$ArtifactSbom"
  "provenance.scan_status=$ArtifactScanStatus"
  "signature.key_id=eva-local-release-signing-key"
  "signature.algorithm=sha256-keyed-v1"
  "signature.value=unavailable"
) -join "`n"
Write-Utf8LfFile $artifactEvidencePath $artifactEvidence

$typedEvidence = @(
  [pscustomobject]@{ Type = "distribution"; Source = $distributionSource; Name = "release-distribution.evidence"; Executor = "release-distribution" },
  [pscustomobject]@{ Type = "security_scan"; Source = $securitySource; Name = "release-security-scan.evidence"; Executor = "release-security-scan" },
  [pscustomobject]@{ Type = "benchmark"; Source = $benchmarkSource; Name = "release-benchmark.evidence"; Executor = "release-benchmark" },
  [pscustomobject]@{ Type = "mcp_compatibility"; Source = $mcpCompatibilitySource; Name = "release-mcp-compatibility.evidence"; Executor = "release-mcp-compatibility" }
)
foreach ($item in $typedEvidence) {
  $destination = Join-Path $productionRoot $item.Name
  if ($item.Type -ceq "mcp_compatibility") {
    [System.IO.File]::Copy($item.Source, $destination, $false)
  } else {
    Copy-CanonicalManifest $item.Source $destination
  }
}

$entries = New-Object System.Collections.Generic.List[object]
$entrySpecs = @(
  [pscustomobject]@{ Type = "artifact"; Evidence = "release-artifact.evidence"; Envelope = "release-artifact.envelope"; Subject = $artifactName; Executor = "release-artifact"; Timestamp = $ArtifactTimestampMilliseconds },
  [pscustomobject]@{ Type = "distribution"; Evidence = "release-distribution.evidence"; Envelope = "release-distribution.envelope"; Subject = $null; Executor = "release-distribution"; Timestamp = $DistributionTimestampMilliseconds },
  [pscustomobject]@{ Type = "security_scan"; Evidence = "release-security-scan.evidence"; Envelope = "release-security-scan.envelope"; Subject = $null; Executor = "release-security-scan"; Timestamp = $SecurityScanTimestampMilliseconds },
  [pscustomobject]@{ Type = "benchmark"; Evidence = "release-benchmark.evidence"; Envelope = "release-benchmark.envelope"; Subject = $null; Executor = "release-benchmark"; Timestamp = $BenchmarkTimestampMilliseconds },
  [pscustomobject]@{ Type = "mcp_compatibility"; Evidence = "release-mcp-compatibility.evidence"; Envelope = "release-mcp-compatibility.envelope"; Subject = $null; Executor = "release-mcp-compatibility"; Timestamp = $McpCompatibilityTimestampMilliseconds }
)
foreach ($spec in $entrySpecs) {
  $evidencePath = Join-Path $productionRoot $spec.Evidence
  $subjectPath = if ($null -eq $spec.Subject) { $evidencePath } else { Join-Path $productionRoot $spec.Subject }
  $subjectDigest = Get-Sha256 ([System.IO.File]::ReadAllBytes($subjectPath))
  $envelopeName = [string]$spec.Envelope
  $envelopePath = Join-Path $productionRoot $envelopeName
  $envelope = @(
    "format=$EnvelopeFormat"
    "kind=measurement"
    "source=release-readback:$($spec.Type)"
    "source_commit=$SourceCommit"
    "environment=github-actions:$Job"
    "executor=github-actions:$($spec.Executor)/$RunId/$RunAttempt/$Job"
    "timestamp=$($spec.Timestamp)"
    "subject_digest=$subjectDigest"
  ) -join "`n"
  Write-Utf8LfFile $envelopePath $envelope
  $entries.Add([pscustomobject]@{
      Type = $spec.Type
      Evidence = $spec.Evidence
      Envelope = $envelopeName
      EnvelopeDigest = Get-Sha256 ([System.IO.File]::ReadAllBytes($envelopePath))
      Subject = $spec.Subject
    })
}

$manifestLines = New-Object System.Collections.Generic.List[string]
$manifestLines.Add("format=$ManifestFormat")
$manifestLines.Add("scope=production")
$manifestLines.Add("source_commit=$SourceCommit")
for ($index = 0; $index -lt $entries.Count; $index += 1) {
  $entry = $entries[$index]
  $manifestLines.Add("entry.$index.type=$($entry.Type)")
  $manifestLines.Add("entry.$index.evidence=$($entry.Evidence)")
  $manifestLines.Add("entry.$index.envelope=$($entry.Envelope)")
  $manifestLines.Add("entry.$index.envelope_digest=$($entry.EnvelopeDigest)")
  if ($null -ne $entry.Subject) {
    $manifestLines.Add("entry.$index.subject=$($entry.Subject)")
  }
}
$manifestPath = Join-Path $productionRoot "release-evidence.manifest"
Write-Utf8LfFile $manifestPath ($manifestLines -join "`n")
$manifestDigest = Get-Sha256 ([System.IO.File]::ReadAllBytes($manifestPath))

$fileMap = New-Object 'System.Collections.Generic.Dictionary[string,string]' ([System.StringComparer]::Ordinal)
foreach ($item in @(Get-ChildItem -LiteralPath $root -Recurse -Force)) {
  $attributes = [System.IO.File]::GetAttributes($item.FullName)
  if (($attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
    Fail-UploadSeal "upload_bundle_path_symlink" $item.FullName
  }
  if ($item.PSIsContainer) {
    continue
  }
  $relative = Get-RelativePath $root $item.FullName
  Assert-SingleLineValue $relative "bundle_relative_path" 1024
  $fileMap.Add($relative, $item.FullName)
}
[string[]]$paths = @($fileMap.Keys)
[System.Array]::Sort($paths, [System.StringComparer]::Ordinal)
if ($paths.Count -eq 0) {
  Fail-UploadSeal "upload_bundle_empty" $root
}

$indexEntries = New-Object System.Collections.Generic.List[object]
$bundlePayload = New-Object System.Text.StringBuilder
foreach ($relative in $paths) {
  $bytes = [System.IO.File]::ReadAllBytes([string]$fileMap[$relative])
  $digest = Get-Sha256 $bytes
  $entry = [pscustomobject]@{ Path = $relative; Size = $bytes.LongLength; Digest = $digest }
  $indexEntries.Add($entry)
  [void]$bundlePayload.Append("path=$relative`nsize_bytes=$($entry.Size)`ndigest=$digest`n")
}
$bundleDigest = Get-Sha256 $Utf8NoBom.GetBytes($bundlePayload.ToString())

$indexLines = New-Object System.Collections.Generic.List[string]
$indexLines.Add("format=$IndexFormat")
$indexLines.Add("source_commit=$SourceCommit")
$indexLines.Add("release_tag=$ReleaseTag")
$indexLines.Add("run_id=$RunId")
$indexLines.Add("run_attempt=$RunAttempt")
$indexLines.Add("producer_job=$Job")
$indexLines.Add("upload_artifact_name=$UploadArtifactName")
$indexLines.Add("manifest_path=production/release-evidence.manifest")
$indexLines.Add("manifest_digest=$manifestDigest")
$indexLines.Add("bundle_digest=$bundleDigest")
$indexLines.Add("entry_count=$($indexEntries.Count)")
for ($index = 0; $index -lt $indexEntries.Count; $index += 1) {
  $entry = $indexEntries[$index]
  $indexLines.Add("entry.$index.path=$($entry.Path)")
  $indexLines.Add("entry.$index.size_bytes=$($entry.Size)")
  $indexLines.Add("entry.$index.digest=$($entry.Digest)")
}
Write-Utf8LfFile $indexPath ($indexLines -join "`n")
$indexDigest = Get-Sha256 ([System.IO.File]::ReadAllBytes($indexPath))

$metadataParent = [System.IO.Path]::GetDirectoryName($metadata)
if (-not [string]::IsNullOrWhiteSpace($metadataParent)) {
  [System.IO.Directory]::CreateDirectory($metadataParent) | Out-Null
}
$metadataValue = [ordered]@{
  schema = "eva.release.evidence_upload_seal.v1"
  upload_artifact_name = $UploadArtifactName
  release_tag = $ReleaseTag
  source_commit = $SourceCommit
  run_id = $RunId
  run_attempt = $RunAttempt
  producer_job = $Job
  index_path = "readback-index.manifest"
  index_digest = $indexDigest
  manifest_path = "production/release-evidence.manifest"
  manifest_digest = $manifestDigest
  bundle_digest = $bundleDigest
  file_count = $indexEntries.Count
}
$metadataJson = ($metadataValue | ConvertTo-Json -Depth 6 -Compress).Replace("`r`n", "`n").Replace("`r", "`n")
[System.IO.File]::WriteAllText($metadata, "$metadataJson`n", $Utf8NoBom)
$metadataValue
