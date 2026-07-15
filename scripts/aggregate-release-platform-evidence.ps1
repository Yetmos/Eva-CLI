[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string[]]$PlatformManifestPath,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$BundleManifestPath,

  [string]$NativeArtifactsPath,

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
  [string]$ExpectedRunAttempt
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$BundleIndexFormat = "eva.release.platform_bundle_index.v1"
$BundleSubjectFormat = "eva.release.platform_bundle.v1"
$EnvelopeFormat = "eva.release.evidence_envelope.v1"
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)

function Fail-PlatformBundle {
  param(
    [string]$Reason,
    [string]$Detail
  )

  $safeDetail = if ([string]::IsNullOrWhiteSpace($Detail)) {
    "none"
  } else {
    $Detail.Replace("`r", " ").Replace("`n", " ")
  }
  throw "[release-platform-bundle] reason=$Reason detail=$safeDetail"
}

function Assert-TrustedInputs {
  if ($ExpectedReleaseTag -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?$') {
    Fail-PlatformBundle "platform_release_tag_invalid" $ExpectedReleaseTag
  }
  if ($ExpectedSourceCommit -cnotmatch '^[0-9a-f]{40}$') {
    Fail-PlatformBundle "platform_source_commit_invalid" $ExpectedSourceCommit
  }
  if ($ExpectedRunId -notmatch '^[1-9][0-9]*$') {
    Fail-PlatformBundle "platform_run_id_invalid" $ExpectedRunId
  }
  if ($ExpectedRunAttempt -notmatch '^[1-9][0-9]*$') {
    Fail-PlatformBundle "platform_run_attempt_invalid" $ExpectedRunAttempt
  }
}

function Get-FullPath {
  param([string]$Path)

  try {
    if ([System.IO.Path]::IsPathRooted($Path)) {
      return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $Path))
  } catch {
    Fail-PlatformBundle "platform_bundle_path_invalid" $Path
  }
}

function Get-PathComparison {
  if ($env:OS -eq "Windows_NT") {
    return [System.StringComparison]::OrdinalIgnoreCase
  }
  return [System.StringComparison]::Ordinal
}

function Assert-SafeOutputPath {
  param(
    [string]$Root,
    [string]$Path,
    [string]$Field
  )

  $rootFull = [System.IO.Path]::GetFullPath($Root).TrimEnd([char[]]@('/', '\'))
  $fullPath = Get-FullPath $Path
  $prefix = "$rootFull$([System.IO.Path]::DirectorySeparatorChar)"
  if (-not $fullPath.StartsWith($prefix, (Get-PathComparison))) {
    Fail-PlatformBundle "platform_bundle_path_escape" $Field
  }
  $relative = $fullPath.Substring($prefix.Length).Replace('\', '/')
  $invalidSegments = @($relative.Split('/') | Where-Object {
      [string]::IsNullOrEmpty($_) -or $_ -eq "." -or $_ -eq ".."
    })
  if ([string]::IsNullOrWhiteSpace($relative) -or $relative.Contains(':') -or
      $invalidSegments.Count -gt 0) {
    Fail-PlatformBundle "platform_bundle_path_invalid" $Field
  }

  $current = $rootFull
  foreach ($segment in $relative.Split('/')) {
    $current = Join-Path $current $segment
    if ([System.IO.File]::Exists($current) -or [System.IO.Directory]::Exists($current)) {
      $attributes = [System.IO.File]::GetAttributes($current)
      if (($attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        Fail-PlatformBundle "platform_bundle_path_symlink" $Field
      }
    }
  }
  if ([System.IO.Directory]::Exists($fullPath)) {
    Fail-PlatformBundle "platform_bundle_path_not_file" $Field
  }
  return [pscustomobject]@{
    FullPath = $fullPath
    RelativePath = $relative
  }
}

function Get-BytesDigest {
  param([byte[]]$Bytes)

  $sha256 = [System.Security.Cryptography.SHA256]::Create()
  try {
    $digest = $sha256.ComputeHash($Bytes)
    return "sha256:$([System.BitConverter]::ToString($digest).Replace('-', '').ToLowerInvariant())"
  } finally {
    $sha256.Dispose()
  }
}

function Write-Utf8LfFile {
  param(
    [string]$Path,
    [string]$Text
  )

  $normalized = $Text.Replace("`r`n", "`n").Replace("`r", "`n")
  [System.IO.File]::WriteAllText($Path, $normalized, $Utf8NoBom)
}

function Write-JsonLfFile {
  param(
    [string]$Path,
    [object]$Value
  )

  $json = ($Value | ConvertTo-Json -Depth 16 -Compress).Replace("`r`n", "`n").Replace("`r", "`n")
  Write-Utf8LfFile $Path "$json`n"
}

function ConvertTo-Hex {
  param([byte[]]$Bytes)

  return [System.BitConverter]::ToString($Bytes).Replace('-', '').ToLowerInvariant()
}

function Add-LengthDelimitedBytes {
  param(
    [System.IO.MemoryStream]$Stream,
    [byte[]]$Bytes
  )

  [byte[]]$lengthBytes = [System.BitConverter]::GetBytes([uint64]$Bytes.LongLength)
  if ([System.BitConverter]::IsLittleEndian) {
    [System.Array]::Reverse($lengthBytes)
  }
  $Stream.Write($lengthBytes, 0, $lengthBytes.Length)
  $Stream.Write($Bytes, 0, $Bytes.Length)
}

function New-BundleManifest {
  param(
    [object[]]$Entries,
    [string]$BundleDigest
  )

  $lines = New-Object System.Collections.Generic.List[string]
  $lines.Add("format=$BundleSubjectFormat")
  $lines.Add("bundle_digest=$BundleDigest")
  $lines.Add("entry_count=$($Entries.Count)")
  for ($index = 0; $index -lt $Entries.Count; $index += 1) {
    $entry = $Entries[$index]
    $lines.Add("entry.$index.subject_hex=$(ConvertTo-Hex $entry.SubjectBytes)")
    $lines.Add("entry.$index.envelope_hex=$(ConvertTo-Hex $entry.EnvelopeBytes)")
  }
  return ($lines -join "`n") + "`n"
}

function New-BundleEnvelope {
  param(
    [int64]$Timestamp,
    [string]$SubjectDigest,
    [int]$EntryCount
  )

  $text = @(
    "format=$EnvelopeFormat"
    "kind=measurement"
    "source=release-platform-bundle"
    "source_commit=$ExpectedSourceCommit"
    "environment=multi-platform:$EntryCount;run_id=$ExpectedRunId;run_attempt=$ExpectedRunAttempt"
    "executor=release-platform-aggregator:run-$ExpectedRunId-attempt-$ExpectedRunAttempt"
    "timestamp=$Timestamp"
    "subject_digest=$SubjectDigest"
  ) -join "`n"
  return "$text`n"
}

function New-LegacyArtifact {
  param([object]$Entry)

  if ($Entry.Os -eq "windows") {
    $install = "Expand-Archive $($Entry.ArchiveName) and run eva.exe --version"
    $uninstall = "Remove the extracted Eva-CLI archive directory"
    $upgrade = "Replace the archive contents and run eva upgrade check --output json"
  } else {
    $install = "tar -xzf $($Entry.ArchiveName) && ./eva --version"
    $uninstall = "remove the extracted Eva-CLI archive directory"
    $upgrade = "replace the archive contents and run eva upgrade check --output json"
  }

  $artifact = [ordered]@{
    target = $Entry.Target
    archive = $Entry.ArchiveName
    format = $Entry.ArchiveFormat
    binary = $Entry.Binary
    checksum = $Entry.ArchiveSha256
    signed = $false
    smoke_test = "passed"
    install_command = $install
    uninstall_command = $uninstall
    upgrade_command = $upgrade
    toolchain = $Entry.Toolchain
    platform_subject_digest = $Entry.SubjectDigest
  }
  if ($Entry.Os -eq "macos") {
    $artifact["notarized"] = $false
  }
  return $artifact
}

Assert-TrustedInputs
if ($PlatformManifestPath.Count -eq 0) {
  Fail-PlatformBundle "platform_bundle_empty" "at least one platform manifest is required"
}

$verifierScript = Join-Path $PSScriptRoot "write-release-platform-evidence.ps1"
if (-not [System.IO.File]::Exists($verifierScript)) {
  Fail-PlatformBundle "platform_verifier_missing" $verifierScript
}

$entriesByTarget = @{}
foreach ($path in $PlatformManifestPath) {
  $manifestFullPath = Get-FullPath $path
  $result = @(& $verifierScript `
      -VerifyManifestPath $manifestFullPath `
      -ExpectedReleaseTag $ExpectedReleaseTag `
      -ExpectedSourceCommit $ExpectedSourceCommit `
      -ExpectedRunId $ExpectedRunId `
      -ExpectedRunAttempt $ExpectedRunAttempt)
  if ($result.Count -ne 1) {
    Fail-PlatformBundle "platform_verifier_result_invalid" $manifestFullPath
  }
  $entry = $result[0]
  if ($entriesByTarget.ContainsKey([string]$entry.Target)) {
    Fail-PlatformBundle "platform_target_duplicate" ([string]$entry.Target)
  }
  $entriesByTarget[[string]$entry.Target] = $entry
}

[string[]]$targets = @($entriesByTarget.Keys)
[System.Array]::Sort($targets, [System.StringComparer]::Ordinal)
$entries = @($targets | ForEach-Object { $entriesByTarget[$_] })

$bundleManifestFullPath = Get-FullPath $BundleManifestPath
$outputRoot = [System.IO.Path]::GetDirectoryName($bundleManifestFullPath)
if ([string]::IsNullOrWhiteSpace($outputRoot)) {
  Fail-PlatformBundle "platform_bundle_path_invalid" $BundleManifestPath
}
[System.IO.Directory]::CreateDirectory($outputRoot) | Out-Null
$bundleManifestOutput = Assert-SafeOutputPath $outputRoot $bundleManifestFullPath "bundle manifest"
$stem = [System.IO.Path]::GetFileNameWithoutExtension($bundleManifestFullPath)
$bundleSubjectOutput = Assert-SafeOutputPath $outputRoot (Join-Path $outputRoot "$stem.subject") "bundle subject"
$bundleEnvelopeOutput = Assert-SafeOutputPath $outputRoot (Join-Path $outputRoot "$stem.envelope") "bundle envelope"
if ([string]::IsNullOrWhiteSpace($NativeArtifactsPath)) {
  $NativeArtifactsPath = Join-Path $outputRoot "native-artifacts.json"
}
$nativeOutput = Assert-SafeOutputPath $outputRoot $NativeArtifactsPath "native artifacts"

$distinctOutputs = @(
  $bundleManifestOutput.FullPath,
  $bundleSubjectOutput.FullPath,
  $bundleEnvelopeOutput.FullPath,
  $nativeOutput.FullPath
) | Select-Object -Unique
if ($distinctOutputs.Count -ne 4) {
  Fail-PlatformBundle "platform_bundle_output_conflict" "output paths must be distinct"
}

$bundlePayload = New-Object System.IO.MemoryStream
try {
  foreach ($entry in $entries) {
    Add-LengthDelimitedBytes $bundlePayload $entry.SubjectBytes
    Add-LengthDelimitedBytes $bundlePayload $entry.EnvelopeBytes
  }
  $bundleDigest = Get-BytesDigest $bundlePayload.ToArray()
} finally {
  $bundlePayload.Dispose()
}
$bundleSubject = New-BundleManifest $entries $bundleDigest
$bundleSubjectBytes = $Utf8NoBom.GetBytes($bundleSubject)
$bundleSubjectDigest = Get-BytesDigest $bundleSubjectBytes
[int64]$timestamp = 0
foreach ($entry in $entries) {
  if ([int64]$entry.Timestamp -gt $timestamp) {
    $timestamp = [int64]$entry.Timestamp
  }
}
$bundleEnvelope = New-BundleEnvelope $timestamp $bundleSubjectDigest $entries.Count
$bundleEnvelopeBytes = $Utf8NoBom.GetBytes($bundleEnvelope)
$bundleEnvelopeDigest = Get-BytesDigest $bundleEnvelopeBytes
Write-Utf8LfFile $bundleSubjectOutput.FullPath $bundleSubject
Write-Utf8LfFile $bundleEnvelopeOutput.FullPath $bundleEnvelope

$legacyArtifacts = @($entries | ForEach-Object { New-LegacyArtifact $_ })
$legacy = [ordered]@{
  status = "published"
  version = $ExpectedReleaseTag.Substring(1)
  source_tag = $ExpectedReleaseTag
  source_sha = $ExpectedSourceCommit
  platform_bundle_digest = $bundleDigest
  artifacts = $legacyArtifacts
  reason = $null
}
Write-JsonLfFile $nativeOutput.FullPath $legacy

$indexEntries = @($entries | ForEach-Object {
    [ordered]@{
      target = $_.Target
      os = $_.Os
      architecture = $_.Architecture
      toolchain = $_.Toolchain
      platform_subject_digest = $_.SubjectDigest
      platform_envelope_digest = $_.EnvelopeDigest
      archive_name = $_.ArchiveName
      archive_byte_count = $_.ArchiveByteCount
      archive_sha256 = $_.ArchiveSha256
    }
  })
$bundleIndex = [ordered]@{
  format = $BundleIndexFormat
  release_tag = $ExpectedReleaseTag
  version = $ExpectedReleaseTag.Substring(1)
  source_commit = $ExpectedSourceCommit
  run_id = $ExpectedRunId
  run_attempt = $ExpectedRunAttempt
  entry_count = $entries.Count
  bundle_digest = $bundleDigest
  entries = $indexEntries
  subject = [ordered]@{
    path = $bundleSubjectOutput.RelativePath
    byte_count = [int64]$bundleSubjectBytes.LongLength
    sha256 = $bundleSubjectDigest
  }
  envelope = [ordered]@{
    path = $bundleEnvelopeOutput.RelativePath
    byte_count = [int64]$bundleEnvelopeBytes.LongLength
    sha256 = $bundleEnvelopeDigest
  }
  native_artifacts = [ordered]@{
    path = $nativeOutput.RelativePath
  }
}
Write-JsonLfFile $bundleManifestOutput.FullPath $bundleIndex
Write-Output $bundleManifestOutput.FullPath
