[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$EvidenceRoot,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedReleaseTag,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedSourceCommit,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedRunId,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedRunAttempt,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedProducerJob,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedUploadArtifactName,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedUploadArtifactId,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedUploadDigest,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedIndexDigest,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedManifestDigest,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedBundleDigest,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedFileCount,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$EvaExecutable,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ReceiptPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$IndexFormat = "eva.release.evidence_readback_index.v1"
$ReceiptFormat = "eva.release.evidence_readback_receipt.v1"
$IndexRelativePath = "readback-index.manifest"
$ManifestRelativePath = "production/release-evidence.manifest"
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false, $true)
$Ordinal = [System.StringComparer]::Ordinal

function Fail-Readback {
  param(
    [string]$Reason,
    [string]$Detail
  )

  $safeDetail = if ([string]::IsNullOrWhiteSpace($Detail)) {
    "none"
  } else {
    $Detail.Replace("`r", " ").Replace("`n", " ")
  }
  throw "[release-evidence-readback] reason=$Reason detail=$safeDetail"
}

function Get-FullPath {
  param([string]$Path)

  try {
    if ([System.IO.Path]::IsPathRooted($Path)) {
      return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $Path))
  } catch {
    Fail-Readback "readback_path_invalid" $Path
  }
}

function Get-PathComparison {
  if ($env:OS -eq "Windows_NT") {
    return [System.StringComparison]::OrdinalIgnoreCase
  }
  return [System.StringComparison]::Ordinal
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
    Fail-Readback $Reason $Path
  }
  $attributes = [System.IO.File]::GetAttributes($Path)
  if (($attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
    Fail-Readback "readback_path_symlink" $Path
  }
}

function Assert-SingleLineValue {
  param(
    [string]$Value,
    [string]$Field,
    [int]$MaximumLength = 1024
  )

  if ([string]::IsNullOrWhiteSpace($Value) -or $Value.Length -gt $MaximumLength -or
      $Value.Contains("`r") -or $Value.Contains("`n") -or $Value.Contains([char]0)) {
    Fail-Readback "readback_field_invalid" $Field
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

function Assert-CanonicalDigest {
  param(
    [string]$Digest,
    [string]$Field
  )

  if ($Digest -cnotmatch '^sha256:[0-9a-f]{64}$') {
    Fail-Readback "readback_digest_invalid" $Field
  }
}

function Normalize-UploadDigest {
  param([string]$Digest)

  if ($Digest -cmatch '^[0-9a-f]{64}$') {
    return $Digest
  }
  Fail-Readback "readback_upload_digest_invalid" "upload digest"
}

function Get-RelativePath {
  param(
    [string]$Root,
    [string]$Path
  )

  if (-not (Test-IsChildPath $Root $Path)) {
    Fail-Readback "readback_path_escape" $Path
  }
  $prefix = $Root.TrimEnd([char[]]@('/', '\')) + [System.IO.Path]::DirectorySeparatorChar
  $relative = $Path.Substring($prefix.Length).Replace('\', '/')
  Assert-SingleLineValue $relative "relative_path"
  if ($relative.Contains(':') -or $relative.Contains('\') -or
      @($relative.Split('/') | Where-Object { [string]::IsNullOrEmpty($_) -or $_ -eq "." -or $_ -eq ".." }).Count -gt 0) {
    Fail-Readback "readback_relative_path_invalid" $relative
  }
  return $relative
}

function Read-StrictIndex {
  param([string]$Path)

  $fields = New-Object 'System.Collections.Generic.Dictionary[string,string]' $Ordinal
  foreach ($rawLine in [System.IO.File]::ReadAllLines($Path, $Utf8NoBom)) {
    $line = $rawLine.TrimStart([char]0xFEFF)
    if ([string]::IsNullOrWhiteSpace($line) -or $line.StartsWith("#")) {
      Fail-Readback "readback_index_noncanonical" $Path
    }
    $separator = $line.IndexOf('=')
    if ($separator -le 0) {
      Fail-Readback "readback_index_invalid" $Path
    }
    $key = $line.Substring(0, $separator)
    $value = $line.Substring($separator + 1)
    Assert-SingleLineValue $key "index_key" 256
    Assert-SingleLineValue $value $key 4096
    if ($fields.ContainsKey($key)) {
      Fail-Readback "readback_index_field_duplicate" $key
    }
    $fields.Add($key, $value)
  }
  return $fields
}

function Get-RequiredField {
  param(
    [System.Collections.Generic.Dictionary[string,string]]$Fields,
    [string]$Name
  )

  if (-not $Fields.ContainsKey($Name)) {
    Fail-Readback "readback_index_field_missing" $Name
  }
  return [string]$Fields[$Name]
}

function ConvertTo-NonNegativeInt64 {
  param(
    [string]$Value,
    [string]$Field
  )

  [long]$parsed = 0
  if (-not [long]::TryParse($Value, [Globalization.NumberStyles]::None, [Globalization.CultureInfo]::InvariantCulture, [ref]$parsed) -or $parsed -lt 0) {
    Fail-Readback "readback_index_number_invalid" $Field
  }
  return $parsed
}

function Write-Utf8LfJson {
  param(
    [string]$Path,
    [object]$Value
  )

  $parent = [System.IO.Path]::GetDirectoryName($Path)
  if (-not [string]::IsNullOrWhiteSpace($parent)) {
    [System.IO.Directory]::CreateDirectory($parent) | Out-Null
  }
  $json = ($Value | ConvertTo-Json -Depth 12 -Compress).Replace("`r`n", "`n").Replace("`r", "`n")
  [System.IO.File]::WriteAllText($Path, "$json`n", $Utf8NoBom)
}

function Assert-IndexedBundleSnapshot {
  param(
    [string]$Root,
    [string]$IndexPath,
    [string]$ExpectedIndexDigest,
    [object]$IndexedFiles,
    [object]$IndexedPaths,
    [string]$ExpectedBundleDigest
  )

  Assert-RegularFile $IndexPath "readback_index_missing"
  $snapshotIndexDigest = Get-Sha256 ([System.IO.File]::ReadAllBytes($IndexPath))
  if ($snapshotIndexDigest -cne $ExpectedIndexDigest) {
    Fail-Readback "readback_index_digest_mismatch" "expected=$ExpectedIndexDigest actual=$snapshotIndexDigest"
  }

  $actualFiles = New-Object 'System.Collections.Generic.Dictionary[string,string]' $Ordinal
  foreach ($item in @(Get-ChildItem -LiteralPath $Root -Recurse -Force)) {
    $attributes = [System.IO.File]::GetAttributes($item.FullName)
    if (($attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
      Fail-Readback "readback_bundle_path_symlink" $item.FullName
    }
    if ($item.PSIsContainer) {
      continue
    }
    $relative = Get-RelativePath $Root $item.FullName
    if ($relative -ceq $IndexRelativePath) {
      continue
    }
    $actualFiles.Add($relative, $item.FullName)
  }
  if ($actualFiles.Count -ne $IndexedFiles.Count) {
    Fail-Readback "readback_file_set_mismatch" "expected=$($IndexedFiles.Count) actual=$($actualFiles.Count)"
  }
  foreach ($relative in $actualFiles.Keys) {
    if (-not $IndexedFiles.ContainsKey($relative)) {
      Fail-Readback "readback_file_unindexed" $relative
    }
  }

  $bundlePayload = New-Object System.Text.StringBuilder
  foreach ($relative in $IndexedPaths) {
    if (-not $actualFiles.ContainsKey($relative)) {
      Fail-Readback "readback_file_missing" $relative
    }
    $expected = $IndexedFiles[$relative]
    Assert-RegularFile $expected.FullPath "readback_file_missing"
    $bytes = [System.IO.File]::ReadAllBytes($expected.FullPath)
    $actualDigest = Get-Sha256 $bytes
    if ($bytes.LongLength -ne $expected.Size) {
      Fail-Readback "readback_file_size_mismatch" $relative
    }
    if ($actualDigest -cne $expected.Digest) {
      Fail-Readback "readback_file_digest_mismatch" $relative
    }
    [void]$bundlePayload.Append("path=$relative`nsize_bytes=$($expected.Size)`ndigest=$actualDigest`n")
  }
  $snapshotBundleDigest = Get-Sha256 $Utf8NoBom.GetBytes($bundlePayload.ToString())
  if ($snapshotBundleDigest -cne $ExpectedBundleDigest) {
    Fail-Readback "readback_bundle_digest_mismatch" "expected=$ExpectedBundleDigest actual=$snapshotBundleDigest"
  }
  return $snapshotBundleDigest
}

if ($ExpectedReleaseTag -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?$') {
  Fail-Readback "readback_release_tag_invalid" $ExpectedReleaseTag
}
if ($ExpectedSourceCommit -cnotmatch '^[0-9a-f]{40}$') {
  Fail-Readback "readback_source_commit_invalid" $ExpectedSourceCommit
}
if ($ExpectedRunId -notmatch '^[1-9][0-9]*$' -or $ExpectedRunAttempt -notmatch '^[1-9][0-9]*$') {
  Fail-Readback "readback_run_identity_invalid" "$ExpectedRunId/$ExpectedRunAttempt"
}
if ($ExpectedProducerJob -notmatch '^[0-9A-Za-z][0-9A-Za-z_.-]*$') {
  Fail-Readback "readback_producer_job_invalid" $ExpectedProducerJob
}
if ($ExpectedUploadArtifactName -notmatch '^[0-9A-Za-z][0-9A-Za-z_.-]{0,127}$') {
  Fail-Readback "readback_upload_artifact_name_invalid" $ExpectedUploadArtifactName
}
if ($ExpectedUploadArtifactId -notmatch '^[1-9][0-9]*$') {
  Fail-Readback "readback_upload_artifact_id_invalid" $ExpectedUploadArtifactId
}
$normalizedUploadDigest = Normalize-UploadDigest $ExpectedUploadDigest
Assert-CanonicalDigest $ExpectedIndexDigest "expected_index_digest"
Assert-CanonicalDigest $ExpectedManifestDigest "expected_manifest_digest"
Assert-CanonicalDigest $ExpectedBundleDigest "expected_bundle_digest"
if ($ExpectedFileCount -notmatch '^[1-9][0-9]*$') {
  Fail-Readback "readback_file_count_invalid" $ExpectedFileCount
}

$root = Get-FullPath $EvidenceRoot
$receipt = Get-FullPath $ReceiptPath
$eva = Get-FullPath $EvaExecutable
if (-not [System.IO.Directory]::Exists($root)) {
  Fail-Readback "readback_root_missing" $root
}
$rootAttributes = [System.IO.File]::GetAttributes($root)
if (($rootAttributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
  Fail-Readback "readback_root_symlink" $root
}
if ((Test-IsChildPath $root $receipt) -or $receipt.Equals($root, (Get-PathComparison))) {
  Fail-Readback "readback_receipt_inside_bundle" $receipt
}
Assert-RegularFile $eva "readback_eva_executable_missing"
if ([System.IO.File]::Exists($receipt)) {
  $receiptAttributes = [System.IO.File]::GetAttributes($receipt)
  if (($receiptAttributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
    Fail-Readback "readback_receipt_symlink" $receipt
  }
}

$indexPath = Join-Path $root $IndexRelativePath
Assert-RegularFile $indexPath "readback_index_missing"
$indexBytes = [System.IO.File]::ReadAllBytes($indexPath)
$actualIndexDigest = Get-Sha256 $indexBytes
if ($actualIndexDigest -cne $ExpectedIndexDigest) {
  Fail-Readback "readback_index_digest_mismatch" "expected=$ExpectedIndexDigest actual=$actualIndexDigest"
}
$fields = Read-StrictIndex $indexPath

$headerNames = @(
  "format",
  "source_commit",
  "release_tag",
  "run_id",
  "run_attempt",
  "producer_job",
  "upload_artifact_name",
  "manifest_path",
  "manifest_digest",
  "bundle_digest",
  "entry_count"
)
foreach ($fieldName in $headerNames) {
  $null = Get-RequiredField $fields $fieldName
}
$headerExpectations = @(
  @("format", $IndexFormat),
  @("source_commit", $ExpectedSourceCommit),
  @("release_tag", $ExpectedReleaseTag),
  @("run_id", $ExpectedRunId),
  @("run_attempt", $ExpectedRunAttempt),
  @("producer_job", $ExpectedProducerJob),
  @("upload_artifact_name", $ExpectedUploadArtifactName),
  @("manifest_path", $ManifestRelativePath),
  @("manifest_digest", $ExpectedManifestDigest),
  @("bundle_digest", $ExpectedBundleDigest)
)
foreach ($expectation in $headerExpectations) {
  $actual = Get-RequiredField $fields ([string]$expectation[0])
  if ($actual -cne [string]$expectation[1]) {
    Fail-Readback "readback_index_identity_mismatch" ([string]$expectation[0])
  }
}

$entryCountValue = ConvertTo-NonNegativeInt64 (Get-RequiredField $fields "entry_count") "entry_count"
if ($entryCountValue -le 0 -or $entryCountValue -gt 100000) {
  Fail-Readback "readback_index_entry_count_invalid" ([string]$entryCountValue)
}
$entryCount = [int]$entryCountValue
if ([string]$entryCount -cne $ExpectedFileCount) {
  Fail-Readback "readback_file_count_mismatch" "expected=$ExpectedFileCount actual=$entryCount"
}
$knownKeys = New-Object 'System.Collections.Generic.HashSet[string]' $Ordinal
foreach ($name in $headerNames) {
  [void]$knownKeys.Add($name)
}
$indexedFiles = New-Object 'System.Collections.Generic.Dictionary[string,object]' $Ordinal
$indexedPaths = New-Object System.Collections.Generic.List[string]
$previousPath = $null
for ($index = 0; $index -lt $entryCount; $index += 1) {
  $pathKey = "entry.$index.path"
  $sizeKey = "entry.$index.size_bytes"
  $digestKey = "entry.$index.digest"
  foreach ($key in @($pathKey, $sizeKey, $digestKey)) {
    [void]$knownKeys.Add($key)
  }
  $relative = Get-RequiredField $fields $pathKey
  $size = ConvertTo-NonNegativeInt64 (Get-RequiredField $fields $sizeKey) $sizeKey
  $digest = Get-RequiredField $fields $digestKey
  Assert-CanonicalDigest $digest $digestKey
  $candidateFullPath = Get-FullPath (Join-Path $root $relative)
  $normalizedRelative = Get-RelativePath $root $candidateFullPath
  if ($normalizedRelative -cne $relative -or $relative -ceq $IndexRelativePath) {
    Fail-Readback "readback_index_path_invalid" $relative
  }
  if ($null -ne $previousPath -and $Ordinal.Compare($previousPath, $relative) -ge 0) {
    Fail-Readback "readback_index_path_order_invalid" $relative
  }
  if ($indexedFiles.ContainsKey($relative)) {
    Fail-Readback "readback_index_path_duplicate" $relative
  }
  $indexedFiles.Add($relative, [pscustomobject]@{
      FullPath = $candidateFullPath
      Size = $size
      Digest = $digest
    })
  $indexedPaths.Add($relative)
  $previousPath = $relative
}
foreach ($key in $fields.Keys) {
  if (-not $knownKeys.Contains($key)) {
    Fail-Readback "readback_index_field_unknown" $key
  }
}
if ($fields.Count -ne $knownKeys.Count) {
  Fail-Readback "readback_index_structure_invalid" "field count"
}

$actualBundleDigest = Assert-IndexedBundleSnapshot $root $indexPath $ExpectedIndexDigest $indexedFiles $indexedPaths $ExpectedBundleDigest

$manifestPath = Join-Path $root $ManifestRelativePath
Assert-RegularFile $manifestPath "readback_manifest_missing"
$actualManifestDigest = Get-Sha256 ([System.IO.File]::ReadAllBytes($manifestPath))
if ($actualManifestDigest -cne $ExpectedManifestDigest) {
  Fail-Readback "readback_manifest_digest_mismatch" "expected=$ExpectedManifestDigest actual=$actualManifestDigest"
}

$receiptParent = [System.IO.Path]::GetDirectoryName($receipt)
if ([string]::IsNullOrWhiteSpace($receiptParent)) {
  Fail-Readback "readback_receipt_path_invalid" $receipt
}
[System.IO.Directory]::CreateDirectory($receiptParent) | Out-Null
$receiptStem = [System.IO.Path]::GetFileNameWithoutExtension($receipt)
$capturePath = Join-Path $receiptParent "$receiptStem.gate.capture.json"
$stdoutPath = Join-Path $receiptParent "$receiptStem.gate.stdout.json"
$stderrPath = Join-Path $receiptParent "$receiptStem.gate.stderr"
foreach ($outputPath in @($capturePath, $stdoutPath, $stderrPath)) {
  $outputFullPath = Get-FullPath $outputPath
  if ((Test-IsChildPath $root $outputFullPath) -or $outputFullPath.Equals($root, (Get-PathComparison))) {
    Fail-Readback "readback_gate_output_inside_bundle" $outputFullPath
  }
}

$captureScript = Join-Path $PSScriptRoot "capture-release-evidence.ps1"
Assert-RegularFile $captureScript "readback_capture_script_missing"
& $captureScript `
  -Executable $eva `
  -ArgumentList @(
    "release", "check",
    "--scope", "production",
    "--evidence-manifest", $manifestPath,
    "--expected-source-commit", $ExpectedSourceCommit,
    "--expected-run-id", $ExpectedRunId,
    "--expected-run-attempt", $ExpectedRunAttempt,
    "--expected-manifest-digest", $ExpectedManifestDigest,
    "--output", "json"
  ) `
  -ManifestPath $capturePath `
  -StdoutPath $stdoutPath `
  -StderrPath $stderrPath `
  -CaptureId "release.readback.production-check" `
  -NoFail | Out-Null

$capture = [System.IO.File]::ReadAllText($capturePath, $Utf8NoBom) | ConvertFrom-Json
if ($capture.outcome -eq "timeout" -or $null -eq $capture.exit_code) {
  Fail-Readback "readback_gate_runtime_failed" ([string]$capture.outcome)
}
$processExitCode = [int]$capture.exit_code
if ($processExitCode -ne 0 -and $processExitCode -ne 3) {
  $errorText = [System.IO.File]::ReadAllText($stderrPath, $Utf8NoBom).Trim()
  Fail-Readback "readback_gate_rejected" "exit=$processExitCode stderr=$errorText"
}
$stderrText = [System.IO.File]::ReadAllText($stderrPath, $Utf8NoBom)
if (-not [string]::IsNullOrWhiteSpace($stderrText)) {
  Fail-Readback "readback_gate_stderr_nonempty" $stderrText
}
$decisionBytes = [System.IO.File]::ReadAllBytes($stdoutPath)
$decisionDigest = Get-Sha256 $decisionBytes
try {
  $decision = $Utf8NoBom.GetString($decisionBytes) | ConvertFrom-Json
} catch {
  Fail-Readback "readback_gate_json_invalid" $_.Exception.Message
}
if ($decision.ok -ne $true -or [string]$decision.command -cne "release.check" -or [int]$decision.exit_code -ne $processExitCode) {
  Fail-Readback "readback_gate_contract_invalid" "top-level contract"
}
if ([string]$decision.data.evidence_scope -cne "production" -or
    [string]$decision.data.version -cne $ExpectedReleaseTag.Substring(1) -or
    [string]$decision.data.target -cne "all" -or
    [string]$decision.data.evidence_manifest.source -cne "manifest" -or
    [int]$decision.data.evidence_manifest.entry_count -ne 5 -or
    [int]$decision.data.evidence_manifest.normalized_envelope_count -ne 5 -or
    [string]$decision.data.evidence_manifest.integrity_status -cne "verified" -or
    [string]$decision.data.evidence_manifest.expected_commit_source -cne "external_option" -or
    [string]$decision.data.evidence_manifest.manifest_digest -cne $ExpectedManifestDigest -or
    [string]$decision.data.evidence_manifest.manifest_digest_source -cne "external_option") {
  Fail-Readback "readback_gate_manifest_contract_invalid" "production manifest summary"
}
$decisionStatus = [string]$decision.data.status
if (($processExitCode -eq 0 -and $decisionStatus -cne "ready") -or
    ($processExitCode -eq 3 -and $decisionStatus -cne "blocked")) {
  Fail-Readback "readback_gate_status_invalid" "exit=$processExitCode status=$decisionStatus"
}

$gateExpectations = @(
  [pscustomobject]@{ Id = "REL-ARTIFACT-PROVENANCE-001"; Type = "artifact"; Executor = "release-artifact" },
  [pscustomobject]@{ Id = "REL-DISTRIBUTION-001"; Type = "distribution"; Executor = "release-distribution" },
  [pscustomobject]@{ Id = "REL-SECURITY-SCAN-001"; Type = "security_scan"; Executor = "release-security-scan" },
  [pscustomobject]@{ Id = "REL-BENCHMARK-001"; Type = "benchmark"; Executor = "release-benchmark" },
  [pscustomobject]@{ Id = "REL-MCP-COMPAT-001"; Type = "mcp_compatibility"; Executor = "release-mcp-compatibility" }
)
foreach ($expectation in $gateExpectations) {
  $matching = @($decision.data.gates | Where-Object { [string]$_.id -ceq $expectation.Id })
  if ($matching.Count -ne 1) {
    Fail-Readback "readback_gate_missing" $expectation.Id
  }
  $gate = $matching[0]
  $provenance = $gate.provenance
  $expectedExecutor = "github-actions:$($expectation.Executor)/$ExpectedRunId/$ExpectedRunAttempt/$ExpectedProducerJob"
  if ([string]$gate.evidence_kind -cne "measurement" -or
      $gate.required -ne $true -or
      [string]$provenance.evidence_type -cne $expectation.Type -or
      [string]$provenance.source_commit -cne $ExpectedSourceCommit -or
      [string]$provenance.executor -cne $expectedExecutor -or
      [string]::IsNullOrWhiteSpace([string]$provenance.source) -or
      [string]::IsNullOrWhiteSpace([string]$provenance.environment) -or
      [long]$provenance.timestamp_ms -le 0) {
    Fail-Readback "readback_gate_provenance_invalid" $expectation.Id
  }
  Assert-CanonicalDigest ([string]$provenance.subject_digest) "$($expectation.Type)_subject_digest"
  Assert-CanonicalDigest ([string]$provenance.envelope_digest) "$($expectation.Type)_envelope_digest"
  $gateStatus = [string]$gate.status
  if ($expectation.Type -eq "artifact") {
    if ($gateStatus -cne "pass" -and $gateStatus -cne "blocked") {
      Fail-Readback "readback_gate_status_invalid" "$($expectation.Id):$gateStatus"
    }
    $artifactEvidence = @($gate.evidence | ForEach-Object { [string]$_ })
    $artifactRemediation = @($gate.remediation | ForEach-Object { [string]$_ })
    if ($gateStatus -eq "pass") {
      if (-not $artifactEvidence.Contains("signature_verified:true") -or
          -not $artifactEvidence.Contains("provenance_verified:true") -or
          $artifactRemediation.Count -ne 0) {
        Fail-Readback "readback_artifact_gate_contract_invalid" "pass"
      }
    } else {
      $allowedRemediation = @(
        "release artifact is marked unsigned",
        "release artifact signature value mismatch"
      )
      if (-not $artifactEvidence.Contains("signature_verified:false") -or
          -not $artifactEvidence.Contains("provenance_verified:true") -or
          $artifactRemediation.Count -ne $allowedRemediation.Count -or
          -not $artifactRemediation.Contains($allowedRemediation[0]) -or
          -not $artifactRemediation.Contains($allowedRemediation[1]) -or
          @($artifactRemediation | Where-Object { -not $allowedRemediation.Contains($_) }).Count -ne 0) {
        Fail-Readback "readback_artifact_blocker_unexpected" ($artifactRemediation -join ",")
      }
    }
  } elseif ($gateStatus -cne "pass") {
    Fail-Readback "readback_gate_status_invalid" "$($expectation.Id):$gateStatus"
  }
}

$actualBundleDigest = Assert-IndexedBundleSnapshot $root $indexPath $ExpectedIndexDigest $indexedFiles $indexedPaths $ExpectedBundleDigest
$actualManifestDigest = Get-Sha256 ([System.IO.File]::ReadAllBytes($manifestPath))
if ($actualManifestDigest -cne $ExpectedManifestDigest) {
  Fail-Readback "readback_manifest_digest_mismatch" "expected=$ExpectedManifestDigest actual=$actualManifestDigest"
}

$receiptValue = [ordered]@{
  schema = $ReceiptFormat
  status = "verified"
  release_tag = $ExpectedReleaseTag
  source_commit = $ExpectedSourceCommit
  run_id = $ExpectedRunId
  run_attempt = $ExpectedRunAttempt
  producer_job = $ExpectedProducerJob
  upload_artifact = [ordered]@{
    name = $ExpectedUploadArtifactName
    id = $ExpectedUploadArtifactId
    digest = $normalizedUploadDigest
  }
  readback = [ordered]@{
    index_path = $IndexRelativePath
    index_digest = $actualIndexDigest
    manifest_path = $ManifestRelativePath
    manifest_digest = $actualManifestDigest
    bundle_digest = $actualBundleDigest
    file_count = $entryCount
  }
  decision = [ordered]@{
    command = "release.check"
    process_exit_code = $processExitCode
    report_exit_code = [int]$decision.exit_code
    status = $decisionStatus
    evidence_scope = [string]$decision.data.evidence_scope
    digest = $decisionDigest
  }
  verified_at = [System.DateTimeOffset]::UtcNow.ToString("o")
}
Write-Utf8LfJson $receipt $receiptValue
$receiptDigest = Get-Sha256 ([System.IO.File]::ReadAllBytes($receipt))
[pscustomobject]@{
  ReceiptPath = $receipt
  ReceiptDigest = $receiptDigest
  IndexDigest = $actualIndexDigest
  ManifestDigest = $actualManifestDigest
  BundleDigest = $actualBundleDigest
  DecisionDigest = $decisionDigest
  DecisionStatus = $decisionStatus
  ProcessExitCode = $processExitCode
}
