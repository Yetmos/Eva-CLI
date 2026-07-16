[CmdletBinding(DefaultParameterSetName = "Write")]
param(
  [Parameter(Mandatory = $true, ParameterSetName = "Write")]
  [ValidateNotNullOrEmpty()]
  [string]$SmokeCapturePath,

  [Parameter(Mandatory = $true, ParameterSetName = "Write")]
  [ValidateNotNullOrEmpty()]
  [string]$ToolchainCapturePath,

  [Parameter(Mandatory = $true, ParameterSetName = "Write")]
  [ValidateNotNullOrEmpty()]
  [string]$ArchivePath,

  [Parameter(Mandatory = $true, ParameterSetName = "Write")]
  [ValidateNotNullOrEmpty()]
  [string]$ManifestPath,

  [Parameter(Mandatory = $true, ParameterSetName = "Write")]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedOs,

  [Parameter(Mandatory = $true, ParameterSetName = "Write")]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedArchitecture,

  [Parameter(Mandatory = $true, ParameterSetName = "Verify")]
  [ValidateNotNullOrEmpty()]
  [string]$VerifyManifestPath,

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

$PlatformFormat = "eva.release.platform_evidence.v1"
$PlatformSubjectFormat = "eva.release.platform_subject.v1"
$EnvelopeFormat = "eva.release.evidence_envelope.v1"
$CaptureFormat = "eva.release.command_capture.v1"
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$StrictUtf8 = New-Object System.Text.UTF8Encoding($false, $true)
$InvariantCulture = [System.Globalization.CultureInfo]::InvariantCulture

function Fail-PlatformEvidence {
  param(
    [string]$Reason,
    [string]$Detail
  )

  $safeDetail = if ([string]::IsNullOrWhiteSpace($Detail)) {
    "none"
  } else {
    $Detail.Replace("`r", " ").Replace("`n", " ")
  }
  throw "[release-platform-evidence] reason=$Reason detail=$safeDetail"
}

function Assert-LineText {
  param(
    [AllowEmptyString()][string]$Value,
    [string]$Field,
    [string]$Reason = "platform_field_invalid"
  )

  if ([string]::IsNullOrWhiteSpace($Value) -or $Value.Trim() -ne $Value -or
      $Value.IndexOf("`r") -ge 0 -or $Value.IndexOf("`n") -ge 0 -or $Value.IndexOf([char]0) -ge 0) {
    Fail-PlatformEvidence $Reason "$Field must be non-empty, trimmed, and fit on one line"
  }
}

function Assert-TrustedInputs {
  if ($ExpectedReleaseTag -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?$') {
    Fail-PlatformEvidence "platform_release_tag_invalid" $ExpectedReleaseTag
  }
  if ($ExpectedSourceCommit -cnotmatch '^[0-9a-f]{40}$') {
    Fail-PlatformEvidence "platform_source_commit_invalid" $ExpectedSourceCommit
  }
  if ($ExpectedRunId -notmatch '^[1-9][0-9]*$') {
    Fail-PlatformEvidence "platform_run_id_invalid" $ExpectedRunId
  }
  if ($ExpectedRunAttempt -notmatch '^[1-9][0-9]*$') {
    Fail-PlatformEvidence "platform_run_attempt_invalid" $ExpectedRunAttempt
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
    Fail-PlatformEvidence "platform_path_invalid" $Path
  }
}

function Get-PathComparison {
  if ($env:OS -eq "Windows_NT") {
    return [System.StringComparison]::OrdinalIgnoreCase
  }
  return [System.StringComparison]::Ordinal
}

function Assert-NoReparsePoint {
  param(
    [string]$Root,
    [string]$RelativePath,
    [string]$Field
  )

  $current = $Root
  $paths = @($Root)
  foreach ($segment in $RelativePath.Split('/')) {
    $current = Join-Path $current $segment
    $paths += $current
  }

  foreach ($path in $paths) {
    if ([System.IO.File]::Exists($path) -or [System.IO.Directory]::Exists($path)) {
      $attributes = [System.IO.File]::GetAttributes($path)
      if (($attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        Fail-PlatformEvidence "platform_path_symlink" "$Field references a symbolic link or reparse point"
      }
    }
  }
}

function Assert-PortableRelativePath {
  param(
    [string]$Path,
    [string]$Field
  )

  Assert-LineText $Path $Field "platform_path_invalid"
  if ([System.IO.Path]::IsPathRooted($Path) -or $Path.Contains('\') -or $Path.Contains(':')) {
    Fail-PlatformEvidence "platform_path_invalid" "$Field must be a portable relative path"
  }
  $segments = $Path.Split('/')
  if ($segments.Count -eq 0) {
    Fail-PlatformEvidence "platform_path_invalid" "$Field is empty"
  }
  foreach ($segment in $segments) {
    if ([string]::IsNullOrEmpty($segment) -or $segment -eq "." -or $segment -eq "..") {
      Fail-PlatformEvidence "platform_path_invalid" "$Field contains an invalid segment"
    }
  }
}

function ConvertTo-RootRelativePath {
  param(
    [string]$Root,
    [string]$FullPath,
    [string]$Field
  )

  $rootFull = [System.IO.Path]::GetFullPath($Root).TrimEnd([char[]]@('/', '\'))
  $candidate = [System.IO.Path]::GetFullPath($FullPath)
  $separator = [System.IO.Path]::DirectorySeparatorChar
  $prefix = "$rootFull$separator"
  if (-not $candidate.StartsWith($prefix, (Get-PathComparison))) {
    Fail-PlatformEvidence "platform_path_escape" "$Field must stay below the platform manifest directory"
  }
  $relative = $candidate.Substring($prefix.Length).Replace('\', '/')
  Assert-PortableRelativePath $relative $Field
  Assert-NoReparsePoint $rootFull $relative $Field
  return $relative
}

function Resolve-PortableReference {
  param(
    [string]$Root,
    [string]$RelativePath,
    [string]$Field,
    [switch]$RequireFile
  )

  Assert-PortableRelativePath $RelativePath $Field
  $fullPath = [System.IO.Path]::GetFullPath((Join-Path $Root $RelativePath))
  $null = ConvertTo-RootRelativePath $Root $fullPath $Field
  if ($RequireFile -and -not [System.IO.File]::Exists($fullPath)) {
    if ([System.IO.Directory]::Exists($fullPath)) {
      Fail-PlatformEvidence "platform_path_not_file" "$Field references a directory"
    }
    Fail-PlatformEvidence "platform_path_not_file" "$Field does not exist"
  }
  return $fullPath
}

function Resolve-InputFileBelowRoot {
  param(
    [string]$Root,
    [string]$Path,
    [string]$Field
  )

  $fullPath = Get-FullPath $Path
  $null = ConvertTo-RootRelativePath $Root $fullPath $Field
  if (-not [System.IO.File]::Exists($fullPath)) {
    if ([System.IO.Directory]::Exists($fullPath)) {
      Fail-PlatformEvidence "platform_path_not_file" "$Field references a directory"
    }
    Fail-PlatformEvidence "platform_path_not_file" "$Field does not exist"
  }
  return $fullPath
}

function Assert-SafeOutputFile {
  param(
    [string]$Root,
    [string]$Path,
    [string]$Field
  )

  $fullPath = Get-FullPath $Path
  $relative = ConvertTo-RootRelativePath $Root $fullPath $Field
  if ([System.IO.Directory]::Exists($fullPath)) {
    Fail-PlatformEvidence "platform_path_not_file" "$Field references a directory"
  }
  Assert-NoReparsePoint $Root $relative $Field
  return $fullPath
}

function Get-FileDigest {
  param([string]$Path)

  $sha256 = [System.Security.Cryptography.SHA256]::Create()
  $stream = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
  try {
    $length = $stream.Length
    $digest = $sha256.ComputeHash($stream)
    return [pscustomobject]@{
      ByteCount = [int64]$length
      Sha256 = "sha256:$([System.BitConverter]::ToString($digest).Replace('-', '').ToLowerInvariant())"
    }
  } finally {
    $stream.Dispose()
    $sha256.Dispose()
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

function Test-ByteArrayEqual {
  param(
    [byte[]]$First,
    [byte[]]$Second
  )

  if ($First.LongLength -ne $Second.LongLength) {
    return $false
  }
  for ([int64]$index = 0; $index -lt $First.LongLength; $index += 1) {
    if ($First[$index] -ne $Second[$index]) {
      return $false
    }
  }
  return $true
}

function Read-StrictUtf8File {
  param(
    [string]$Path,
    [string]$Reason
  )

  $bytes = [System.IO.File]::ReadAllBytes($Path)
  try {
    $text = $StrictUtf8.GetString($bytes)
  } catch {
    Fail-PlatformEvidence $Reason "$Path is not valid UTF-8"
  }
  return [pscustomobject]@{
    Bytes = $bytes
    Text = $text
    ByteCount = [int64]$bytes.LongLength
    Sha256 = Get-BytesDigest $bytes
  }
}

function Read-JsonObject {
  param(
    [string]$Path,
    [string]$Reason
  )

  $file = Read-StrictUtf8File $Path $Reason
  try {
    $convertFromJson = Get-Command ConvertFrom-Json -ErrorAction Stop
    $value = if ($convertFromJson.Parameters.ContainsKey("DateKind")) {
      $file.Text | ConvertFrom-Json -DateKind String -ErrorAction Stop
    } else {
      $file.Text | ConvertFrom-Json -ErrorAction Stop
    }
  } catch {
    Fail-PlatformEvidence $Reason "$Path is not valid JSON"
  }
  if ($null -eq $value -or $value -is [System.Array] -or $value -is [string] -or $value -is [ValueType]) {
    Fail-PlatformEvidence $Reason "$Path must contain a JSON object"
  }
  return [pscustomobject]@{
    Value = $value
    Bytes = $file.Bytes
    ByteCount = $file.ByteCount
    Sha256 = $file.Sha256
  }
}

function Get-RequiredProperty {
  param(
    [object]$Object,
    [string]$Name,
    [string]$Reason
  )

  if ($null -eq $Object) {
    Fail-PlatformEvidence $Reason "missing object for property $Name"
  }
  $property = $Object.PSObject.Properties[$Name]
  if ($null -eq $property) {
    Fail-PlatformEvidence $Reason "missing property $Name"
  }
  return $property.Value
}

function ConvertTo-NonNegativeInt64 {
  param(
    [object]$Value,
    [string]$Field,
    [string]$Reason
  )

  if ($null -eq $Value) {
    Fail-PlatformEvidence $Reason "$Field is null"
  }
  $text = [System.Convert]::ToString($Value, $InvariantCulture)
  [int64]$parsed = 0
  if (-not [int64]::TryParse($text, [System.Globalization.NumberStyles]::None, $InvariantCulture, [ref]$parsed) -or $parsed -lt 0) {
    Fail-PlatformEvidence $Reason "$Field must be a non-negative integer"
  }
  return $parsed
}

function ConvertTo-FinishedAt {
  param([object]$Value)

  if ($null -eq $Value) {
    Fail-PlatformEvidence "platform_capture_timestamp_invalid" "capture.finished_at is null"
  }
  $text = if ($Value -is [System.DateTimeOffset]) {
    $Value.ToUniversalTime().ToString("o", $InvariantCulture)
  } elseif ($Value -is [System.DateTime]) {
    if ($Value.Kind -eq [System.DateTimeKind]::Unspecified) {
      Fail-PlatformEvidence "platform_capture_timestamp_invalid" "capture.finished_at has no timezone"
    }
    $Value.ToUniversalTime().ToString("o", $InvariantCulture)
  } else {
    [System.Convert]::ToString($Value, $InvariantCulture)
  }
  Assert-LineText $text "capture.finished_at" "platform_capture_timestamp_invalid"
  [System.DateTimeOffset]$parsed = [System.DateTimeOffset]::MinValue
  if (-not [System.DateTimeOffset]::TryParse($text, $InvariantCulture, [System.Globalization.DateTimeStyles]::RoundtripKind, [ref]$parsed)) {
    Fail-PlatformEvidence "platform_capture_timestamp_invalid" $text
  }
  if ($parsed.Offset -ne [System.TimeSpan]::Zero) {
    Fail-PlatformEvidence "platform_capture_timestamp_invalid" "finished_at must use UTC"
  }
  $utc = $parsed.ToUniversalTime()
  $epochTicks = [System.DateTimeOffset]::new(1970, 1, 1, 0, 0, 0, [System.TimeSpan]::Zero).Ticks
  $milliseconds = [int64](($utc.Ticks - $epochTicks) / [System.TimeSpan]::TicksPerMillisecond)
  if ($milliseconds -le 0) {
    Fail-PlatformEvidence "platform_capture_timestamp_invalid" "finished_at must be after the Unix epoch"
  }
  return [pscustomobject]@{
    Canonical = $utc.ToString("o", $InvariantCulture)
    EpochMilliseconds = $milliseconds
  }
}

function Normalize-Os {
  param([string]$Value)

  Assert-LineText $Value "platform.os"
  switch ($Value.ToLowerInvariant()) {
    "windows" { return "windows" }
    "linux" { return "linux" }
    "macos" { return "macos" }
    "mac" { return "macos" }
    "darwin" { return "macos" }
    default { Fail-PlatformEvidence "platform_os_invalid" $Value }
  }
}

function Normalize-Architecture {
  param([string]$Value)

  Assert-LineText $Value "platform.architecture"
  switch ($Value.ToLowerInvariant()) {
    "x64" { return "x86_64" }
    "amd64" { return "x86_64" }
    "x86_64" { return "x86_64" }
    "arm64" { return "aarch64" }
    "aarch64" { return "aarch64" }
    default { Fail-PlatformEvidence "platform_architecture_invalid" $Value }
  }
}

function Get-PlatformPolicy {
  param(
    [string]$Os,
    [string]$Architecture,
    [string]$Version
  )

  if ($Os -eq "windows" -and $Architecture -eq "x86_64") {
    return [pscustomobject]@{
      Target = "x86_64-pc-windows-msvc"
      Archive = "eva-cli-$Version-x86_64-pc-windows-msvc.zip"
      Format = "zip"
      Binary = "eva.exe"
    }
  }
  if ($Os -eq "linux" -and $Architecture -eq "x86_64") {
    return [pscustomobject]@{
      Target = "x86_64-unknown-linux-gnu"
      Archive = "eva-cli-$Version-x86_64-unknown-linux-gnu.tar.gz"
      Format = "tar.gz"
      Binary = "eva"
    }
  }
  if ($Os -eq "linux" -and $Architecture -eq "aarch64") {
    return [pscustomobject]@{
      Target = "aarch64-unknown-linux-gnu"
      Archive = "eva-cli-$Version-aarch64-unknown-linux-gnu.tar.gz"
      Format = "tar.gz"
      Binary = "eva"
    }
  }
  if ($Os -eq "macos" -and $Architecture -eq "x86_64") {
    return [pscustomobject]@{
      Target = "x86_64-apple-darwin"
      Archive = "eva-cli-$Version-x86_64-apple-darwin.tar.gz"
      Format = "tar.gz"
      Binary = "eva"
    }
  }
  if ($Os -eq "macos" -and $Architecture -eq "aarch64") {
    return [pscustomobject]@{
      Target = "aarch64-apple-darwin"
      Archive = "eva-cli-$Version-aarch64-apple-darwin.tar.gz"
      Format = "tar.gz"
      Binary = "eva"
    }
  }
  Fail-PlatformEvidence "platform_target_unsupported" "$Os/$Architecture"
}

function Assert-CaptureCommand {
  param(
    [object]$Capture,
    [string]$Role
  )

  $executable = [string](Get-RequiredProperty $Capture "executable" "platform_capture_command_invalid")
  Assert-LineText $executable "capture.executable" "platform_capture_command_invalid"
  $portableExecutable = $executable.Replace('\', '/')
  $leaf = $portableExecutable.Substring($portableExecutable.LastIndexOf('/') + 1).ToLowerInvariant()
  $expectedLeafs = if ($Role -eq "smoke") { @("eva", "eva.exe") } else { @("rustc", "rustc.exe") }
  if ($expectedLeafs -notcontains $leaf) {
    Fail-PlatformEvidence "platform_capture_command_invalid" "$Role executable is $leaf"
  }

  $argv = @(Get-RequiredProperty $Capture "argv" "platform_capture_command_invalid")
  if ($argv.Count -ne 1 -or [string]$argv[0] -ne "--version") {
    Fail-PlatformEvidence "platform_capture_command_invalid" "$Role argv must be [--version]"
  }
}

function Read-CaptureStream {
  param(
    [object]$StreamClaim,
    [string]$CaptureDirectory,
    [string]$PlatformRoot,
    [string]$Field
  )

  $relativePath = [string](Get-RequiredProperty $StreamClaim "path" "platform_capture_stream_invalid")
  $streamPath = Resolve-PortableReference $CaptureDirectory $relativePath "$Field.path" -RequireFile
  $rootRelativePath = ConvertTo-RootRelativePath $PlatformRoot $streamPath "$Field.path"
  $actual = Read-StrictUtf8File $streamPath "platform_capture_stream_utf8_invalid"
  $claimedSize = ConvertTo-NonNegativeInt64 (Get-RequiredProperty $StreamClaim "byte_count" "platform_capture_stream_invalid") "$Field.byte_count" "platform_capture_stream_invalid"
  $claimedDigest = [string](Get-RequiredProperty $StreamClaim "sha256" "platform_capture_stream_invalid")
  if ($claimedSize -ne $actual.ByteCount) {
    Fail-PlatformEvidence "platform_capture_stream_size_mismatch" $Field
  }
  if ($claimedDigest -cne $actual.Sha256) {
    Fail-PlatformEvidence "platform_capture_stream_digest_mismatch" $Field
  }
  return [pscustomobject]@{
    Path = $rootRelativePath
    Bytes = $actual.Bytes
    Text = $actual.Text
    ByteCount = $actual.ByteCount
    Sha256 = $actual.Sha256
  }
}

function Read-CaptureEvidence {
  param(
    [string]$CapturePath,
    [string]$PlatformRoot,
    [string]$Role,
    [string]$ExpectedPlatformOs,
    [string]$ExpectedPlatformArchitecture
  )

  $captureFullPath = Resolve-InputFileBelowRoot $PlatformRoot $CapturePath "$Role capture"
  $captureDirectory = [System.IO.Path]::GetDirectoryName($captureFullPath)
  $captureFile = Read-JsonObject $captureFullPath "platform_capture_json_invalid"
  $capture = $captureFile.Value
  if ([string](Get-RequiredProperty $capture "format" "platform_capture_format_invalid") -cne $CaptureFormat) {
    Fail-PlatformEvidence "platform_capture_format_invalid" $Role
  }
  if ([string](Get-RequiredProperty $capture "outcome" "platform_capture_outcome_invalid") -cne "success") {
    Fail-PlatformEvidence "platform_capture_outcome_invalid" "$Role capture did not succeed"
  }
  $exitCode = ConvertTo-NonNegativeInt64 (Get-RequiredProperty $capture "exit_code" "platform_capture_outcome_invalid") "$Role.exit_code" "platform_capture_outcome_invalid"
  if ($exitCode -ne 0) {
    Fail-PlatformEvidence "platform_capture_outcome_invalid" "$Role exit_code is not zero"
  }
  Assert-CaptureCommand $capture $Role

  $runner = Get-RequiredProperty $capture "runner" "platform_capture_runner_invalid"
  $runId = [string](Get-RequiredProperty $runner "run_id" "platform_capture_runner_invalid")
  $runAttempt = [string](Get-RequiredProperty $runner "run_attempt" "platform_capture_runner_invalid")
  if ($runId -cne $ExpectedRunId) {
    Fail-PlatformEvidence "platform_capture_run_id_mismatch" "$Role run_id=$runId"
  }
  if ($runAttempt -cne $ExpectedRunAttempt) {
    Fail-PlatformEvidence "platform_capture_run_attempt_mismatch" "$Role run_attempt=$runAttempt"
  }
  $runnerOs = Normalize-Os ([string](Get-RequiredProperty $runner "os" "platform_capture_runner_invalid"))
  $runnerArchitecture = Normalize-Architecture ([string](Get-RequiredProperty $runner "architecture" "platform_capture_runner_invalid"))
  if ($runnerOs -cne $ExpectedPlatformOs) {
    Fail-PlatformEvidence "platform_capture_os_mismatch" "$Role os=$runnerOs"
  }
  if ($runnerArchitecture -cne $ExpectedPlatformArchitecture) {
    Fail-PlatformEvidence "platform_capture_architecture_mismatch" "$Role architecture=$runnerArchitecture"
  }

  $runnerProvider = [string](Get-RequiredProperty $runner "provider" "platform_capture_runner_invalid")
  $runnerIdentity = [string](Get-RequiredProperty $runner "identity" "platform_capture_runner_invalid")
  $runnerName = [string](Get-RequiredProperty $runner "name" "platform_capture_runner_invalid")
  $runnerJob = [string](Get-RequiredProperty $runner "job" "platform_capture_runner_invalid")
  foreach ($pair in @(
      @("runner.provider", $runnerProvider),
      @("runner.identity", $runnerIdentity),
      @("runner.name", $runnerName),
      @("runner.job", $runnerJob)
    )) {
    Assert-LineText ([string]$pair[1]) ([string]$pair[0]) "platform_capture_runner_invalid"
  }
  $expectedRunnerIdentity = "$runnerName/$runId/$runAttempt/$runnerJob"
  if ($runnerProvider -cne "github-actions" -or $runnerIdentity -cne $expectedRunnerIdentity) {
    Fail-PlatformEvidence "platform_capture_runner_invalid" "$Role runner identity is not canonical"
  }

  $stdout = Read-CaptureStream (Get-RequiredProperty $capture "stdout" "platform_capture_stream_invalid") $captureDirectory $PlatformRoot "$Role.stdout"
  $stderr = Read-CaptureStream (Get-RequiredProperty $capture "stderr" "platform_capture_stream_invalid") $captureDirectory $PlatformRoot "$Role.stderr"
  $finishedAt = ConvertTo-FinishedAt (Get-RequiredProperty $capture "finished_at" "platform_capture_timestamp_invalid")
  $duration = ConvertTo-NonNegativeInt64 (Get-RequiredProperty $capture "duration_ms" "platform_capture_duration_invalid") "$Role.duration_ms" "platform_capture_duration_invalid"
  $captureId = [string](Get-RequiredProperty $capture "capture_id" "platform_capture_id_invalid")
  Assert-LineText $captureId "$Role.capture_id" "platform_capture_id_invalid"

  return [pscustomobject]@{
    Role = $Role
    CaptureId = $captureId
    ManifestPath = ConvertTo-RootRelativePath $PlatformRoot $captureFullPath "$Role capture"
    ManifestByteCount = $captureFile.ByteCount
    ManifestSha256 = $captureFile.Sha256
    FinishedAt = $finishedAt.Canonical
    FinishedAtEpochMilliseconds = $finishedAt.EpochMilliseconds
    DurationMilliseconds = $duration
    Runner = [pscustomobject]@{
      Provider = $runnerProvider
      Identity = $runnerIdentity
      Name = $runnerName
      Job = $runnerJob
      Os = $runnerOs
      Architecture = $runnerArchitecture
      RunId = $runId
      RunAttempt = $runAttempt
    }
    Stdout = $stdout
    Stderr = $stderr
  }
}

function Assert-CaptureRunnerConsistency {
  param(
    [object]$Smoke,
    [object]$Toolchain
  )

  foreach ($field in @("Provider", "Identity", "Name", "Job", "Os", "Architecture", "RunId", "RunAttempt")) {
    if ([string]$Smoke.Runner.$field -cne [string]$Toolchain.Runner.$field) {
      Fail-PlatformEvidence "platform_capture_runner_conflict" $field
    }
  }
}

function Get-ToolchainLine {
  param([string]$Text)

  $normalized = $Text.Replace("`r`n", "`n").Replace("`r", "`n").TrimEnd([char[]]@("`n"))
  Assert-LineText $normalized "toolchain.stdout" "platform_toolchain_output_invalid"
  if (-not $normalized.StartsWith("rustc ", [System.StringComparison]::Ordinal)) {
    Fail-PlatformEvidence "platform_toolchain_output_invalid" $normalized
  }
  return $normalized
}

function Assert-SmokeVersion {
  param(
    [string]$Text,
    [string]$Version
  )

  $normalized = $Text.Replace("`r`n", "`n").Replace("`r", "`n").TrimEnd([char[]]@("`n"))
  if ([string]::IsNullOrEmpty($normalized)) {
    Fail-PlatformEvidence "platform_smoke_version_mismatch" "smoke stdout is empty"
  }
  $lines = $normalized.Split([char]"`n")
  if ($lines[0] -cne "eva $Version") {
    Fail-PlatformEvidence "platform_smoke_version_mismatch" $lines[0]
  }
}

function Convert-CaptureToIndex {
  param([object]$Capture)

  return [ordered]@{
    manifest_path = $Capture.ManifestPath
    manifest_byte_count = $Capture.ManifestByteCount
    manifest_sha256 = $Capture.ManifestSha256
    capture_id = $Capture.CaptureId
    finished_at = $Capture.FinishedAt
    duration_ms = $Capture.DurationMilliseconds
    stdout = [ordered]@{
      path = $Capture.Stdout.Path
      byte_count = $Capture.Stdout.ByteCount
      sha256 = $Capture.Stdout.Sha256
    }
    stderr = [ordered]@{
      path = $Capture.Stderr.Path
      byte_count = $Capture.Stderr.ByteCount
      sha256 = $Capture.Stderr.Sha256
    }
  }
}

function New-PlatformSubject {
  param(
    [string]$ReleaseTag,
    [string]$Version,
    [string]$SourceCommit,
    [string]$RunId,
    [string]$RunAttempt,
    [string]$Os,
    [string]$Architecture,
    [object]$Policy,
    [string]$Toolchain,
    [object]$Runner,
    [object]$Archive,
    [object]$Smoke,
    [object]$ToolchainCapture
  )

  $lines = New-Object System.Collections.Generic.List[string]
  $lines.Add("format=$PlatformSubjectFormat")
  $lines.Add("tag=$ReleaseTag")
  $lines.Add("commit=$SourceCommit")
  $lines.Add("os=$Os")
  $lines.Add("arch=$Architecture")
  $lines.Add("toolchain=$Toolchain")
  $lines.Add("run_id=$RunId")
  $lines.Add("run_attempt=$RunAttempt")
  $lines.Add("job=$($Runner.Job)")
  $lines.Add("artifact.name=$($Policy.Archive)")
  $lines.Add("artifact.target=$($Policy.Target)")
  $lines.Add("artifact.digest=$($Archive.Sha256)")
  $lines.Add("artifact.size_bytes=$($Archive.ByteCount)")
  foreach ($captureEntry in @(
      [pscustomobject]@{ Prefix = "capture"; Value = $Smoke },
      [pscustomobject]@{ Prefix = "toolchain_capture"; Value = $ToolchainCapture }
    )) {
    $prefix = $captureEntry.Prefix
    $capture = $captureEntry.Value
    $lines.Add("$prefix.id=$($capture.CaptureId)")
    $lines.Add("$prefix.outcome=success")
    $lines.Add("$prefix.manifest_digest=$($capture.ManifestSha256)")
    $lines.Add("$prefix.manifest_size=$($capture.ManifestByteCount)")
    $lines.Add("$prefix.stdout_digest=$($capture.Stdout.Sha256)")
    $lines.Add("$prefix.stdout_size=$($capture.Stdout.ByteCount)")
    $lines.Add("$prefix.stderr_digest=$($capture.Stderr.Sha256)")
    $lines.Add("$prefix.stderr_size=$($capture.Stderr.ByteCount)")
  }
  return ($lines -join "`n") + "`n"
}

function New-EvidenceEnvelope {
  param(
    [string]$SourceCommit,
    [string]$Target,
    [string]$Os,
    [string]$Architecture,
    [string]$Toolchain,
    [string]$Executor,
    [int64]$Timestamp,
    [string]$SubjectDigest
  )

  foreach ($pair in @(
      @("source", "release-platform:$Target"),
      @("environment", "$Os-$Architecture;$Toolchain"),
      @("executor", $Executor)
    )) {
    Assert-LineText ([string]$pair[1]) ([string]$pair[0])
  }
  $text = @(
    "format=$EnvelopeFormat"
    "kind=measurement"
    "source=release-platform:$Target"
    "source_commit=$SourceCommit"
    "environment=$Os-$Architecture;$Toolchain"
    "executor=$Executor"
    "timestamp=$Timestamp"
    "subject_digest=$SubjectDigest"
  ) -join "`n"
  return "$text`n"
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

function Assert-ClaimEqual {
  param(
    [object]$Claim,
    [object]$Actual,
    [string]$Reason,
    [string]$Field
  )

  if ([string]$Claim -cne [string]$Actual) {
    Fail-PlatformEvidence $Reason $Field
  }
}

function Assert-CaptureIndex {
  param(
    [object]$Claim,
    [object]$Actual,
    [string]$Role
  )

  foreach ($mapping in @(
      @("manifest_path", $Actual.ManifestPath),
      @("manifest_byte_count", $Actual.ManifestByteCount),
      @("manifest_sha256", $Actual.ManifestSha256),
      @("capture_id", $Actual.CaptureId),
      @("duration_ms", $Actual.DurationMilliseconds)
    )) {
    Assert-ClaimEqual (Get-RequiredProperty $Claim ([string]$mapping[0]) "platform_index_capture_invalid") $mapping[1] "platform_index_capture_mismatch" "$Role.$($mapping[0])"
  }
  $claimedFinishedAt = ConvertTo-FinishedAt (Get-RequiredProperty $Claim "finished_at" "platform_index_capture_invalid")
  Assert-ClaimEqual $claimedFinishedAt.Canonical $Actual.FinishedAt "platform_index_capture_mismatch" "$Role.finished_at"
  foreach ($streamName in @("stdout", "stderr")) {
    $streamClaim = Get-RequiredProperty $Claim $streamName "platform_index_capture_invalid"
    $actualStream = $Actual.$streamName
    foreach ($mapping in @(
        @("path", $actualStream.Path),
        @("byte_count", $actualStream.ByteCount),
        @("sha256", $actualStream.Sha256)
      )) {
      Assert-ClaimEqual (Get-RequiredProperty $streamClaim ([string]$mapping[0]) "platform_index_capture_invalid") $mapping[1] "platform_index_capture_mismatch" "$Role.$streamName.$($mapping[0])"
    }
  }
}

function Read-AndVerifyPlatformManifest {
  param([string]$Path)

  $manifestFullPath = Get-FullPath $Path
  if (-not [System.IO.File]::Exists($manifestFullPath)) {
    Fail-PlatformEvidence "platform_manifest_not_file" $Path
  }
  $root = [System.IO.Path]::GetDirectoryName($manifestFullPath)
  $manifestRelative = [System.IO.Path]::GetFileName($manifestFullPath)
  Assert-NoReparsePoint $root $manifestRelative "platform manifest"
  $indexFile = Read-JsonObject $manifestFullPath "platform_manifest_json_invalid"
  $index = $indexFile.Value
  if ([string](Get-RequiredProperty $index "format" "platform_manifest_format_invalid") -cne $PlatformFormat) {
    Fail-PlatformEvidence "platform_manifest_format_invalid" $Path
  }
  Assert-ClaimEqual (Get-RequiredProperty $index "release_tag" "platform_manifest_invalid") $ExpectedReleaseTag "platform_release_tag_mismatch" "release_tag"
  Assert-ClaimEqual (Get-RequiredProperty $index "source_commit" "platform_manifest_invalid") $ExpectedSourceCommit "platform_source_commit_mismatch" "source_commit"
  Assert-ClaimEqual (Get-RequiredProperty $index "run_id" "platform_manifest_invalid") $ExpectedRunId "platform_run_id_mismatch" "run_id"
  Assert-ClaimEqual (Get-RequiredProperty $index "run_attempt" "platform_manifest_invalid") $ExpectedRunAttempt "platform_run_attempt_mismatch" "run_attempt"

  $version = $ExpectedReleaseTag.Substring(1)
  Assert-ClaimEqual (Get-RequiredProperty $index "version" "platform_manifest_invalid") $version "platform_release_tag_mismatch" "version"
  $platform = Get-RequiredProperty $index "platform" "platform_manifest_invalid"
  $os = Normalize-Os ([string](Get-RequiredProperty $platform "os" "platform_manifest_invalid"))
  $architecture = Normalize-Architecture ([string](Get-RequiredProperty $platform "architecture" "platform_manifest_invalid"))
  $policy = Get-PlatformPolicy $os $architecture $version
  Assert-ClaimEqual (Get-RequiredProperty $platform "target" "platform_manifest_invalid") $policy.Target "platform_target_mismatch" "platform.target"

  $captures = Get-RequiredProperty $index "captures" "platform_manifest_invalid"
  $smokeClaim = Get-RequiredProperty $captures "smoke" "platform_manifest_invalid"
  $toolchainClaim = Get-RequiredProperty $captures "toolchain" "platform_manifest_invalid"
  $smokePath = Resolve-PortableReference $root ([string](Get-RequiredProperty $smokeClaim "manifest_path" "platform_index_capture_invalid")) "captures.smoke.manifest_path" -RequireFile
  $toolchainPath = Resolve-PortableReference $root ([string](Get-RequiredProperty $toolchainClaim "manifest_path" "platform_index_capture_invalid")) "captures.toolchain.manifest_path" -RequireFile
  $smoke = Read-CaptureEvidence $smokePath $root "smoke" $os $architecture
  $toolchainCapture = Read-CaptureEvidence $toolchainPath $root "toolchain" $os $architecture
  Assert-CaptureRunnerConsistency $smoke $toolchainCapture
  Assert-CaptureIndex $smokeClaim $smoke "smoke"
  Assert-CaptureIndex $toolchainClaim $toolchainCapture "toolchain"
  Assert-SmokeVersion $smoke.Stdout.Text $version
  $toolchain = Get-ToolchainLine $toolchainCapture.Stdout.Text
  Assert-ClaimEqual (Get-RequiredProperty $platform "toolchain" "platform_manifest_invalid") $toolchain "platform_toolchain_mismatch" "platform.toolchain"

  $archiveClaim = Get-RequiredProperty $index "archive" "platform_manifest_invalid"
  $archivePath = Resolve-PortableReference $root ([string](Get-RequiredProperty $archiveClaim "path" "platform_archive_invalid")) "archive.path" -RequireFile
  $archiveName = [System.IO.Path]::GetFileName($archivePath)
  Assert-ClaimEqual $archiveName $policy.Archive "platform_archive_name_mismatch" "archive.name"
  Assert-ClaimEqual (Get-RequiredProperty $archiveClaim "name" "platform_archive_invalid") $policy.Archive "platform_archive_name_mismatch" "archive.name"
  Assert-ClaimEqual (Get-RequiredProperty $archiveClaim "format" "platform_archive_invalid") $policy.Format "platform_archive_format_mismatch" "archive.format"
  Assert-ClaimEqual (Get-RequiredProperty $archiveClaim "binary" "platform_archive_invalid") $policy.Binary "platform_archive_binary_mismatch" "archive.binary"
  $archiveActual = Get-FileDigest $archivePath
  Assert-ClaimEqual (Get-RequiredProperty $archiveClaim "byte_count" "platform_archive_invalid") $archiveActual.ByteCount "platform_archive_size_mismatch" "archive.byte_count"
  Assert-ClaimEqual (Get-RequiredProperty $archiveClaim "sha256" "platform_archive_invalid") $archiveActual.Sha256 "platform_archive_digest_mismatch" "archive.sha256"
  $archive = [pscustomobject]@{
    Path = ConvertTo-RootRelativePath $root $archivePath "archive.path"
    ByteCount = $archiveActual.ByteCount
    Sha256 = $archiveActual.Sha256
  }

  $subjectText = New-PlatformSubject $ExpectedReleaseTag $version $ExpectedSourceCommit $ExpectedRunId $ExpectedRunAttempt $os $architecture $policy $toolchain $smoke.Runner $archive $smoke $toolchainCapture
  $subjectBytes = $Utf8NoBom.GetBytes($subjectText)
  $subjectDigest = Get-BytesDigest $subjectBytes
  $subjectClaim = Get-RequiredProperty $index "subject" "platform_manifest_invalid"
  $subjectPath = Resolve-PortableReference $root ([string](Get-RequiredProperty $subjectClaim "path" "platform_subject_invalid")) "subject.path" -RequireFile
  $storedSubject = Read-StrictUtf8File $subjectPath "platform_subject_utf8_invalid"
  if (-not (Test-ByteArrayEqual $storedSubject.Bytes $subjectBytes)) {
    Fail-PlatformEvidence "platform_subject_content_mismatch" $policy.Target
  }
  Assert-ClaimEqual (Get-RequiredProperty $subjectClaim "byte_count" "platform_subject_invalid") $storedSubject.ByteCount "platform_subject_size_mismatch" "subject.byte_count"
  Assert-ClaimEqual (Get-RequiredProperty $subjectClaim "sha256" "platform_subject_invalid") $subjectDigest "platform_subject_digest_mismatch" "subject.sha256"

  $timestamp = [System.Math]::Max($smoke.FinishedAtEpochMilliseconds, $toolchainCapture.FinishedAtEpochMilliseconds)
  $envelopeText = New-EvidenceEnvelope $ExpectedSourceCommit $policy.Target $os $architecture $toolchain $smoke.Runner.Identity $timestamp $subjectDigest
  $envelopeBytes = $Utf8NoBom.GetBytes($envelopeText)
  $envelopeDigest = Get-BytesDigest $envelopeBytes
  $envelopeClaim = Get-RequiredProperty $index "envelope" "platform_manifest_invalid"
  $envelopePath = Resolve-PortableReference $root ([string](Get-RequiredProperty $envelopeClaim "path" "platform_envelope_invalid")) "envelope.path" -RequireFile
  $storedEnvelope = Read-StrictUtf8File $envelopePath "platform_envelope_utf8_invalid"
  if (-not (Test-ByteArrayEqual $storedEnvelope.Bytes $envelopeBytes)) {
    Fail-PlatformEvidence "platform_envelope_content_mismatch" $policy.Target
  }
  Assert-ClaimEqual (Get-RequiredProperty $envelopeClaim "byte_count" "platform_envelope_invalid") $storedEnvelope.ByteCount "platform_envelope_size_mismatch" "envelope.byte_count"
  Assert-ClaimEqual (Get-RequiredProperty $envelopeClaim "sha256" "platform_envelope_invalid") $envelopeDigest "platform_envelope_digest_mismatch" "envelope.sha256"

  return [pscustomobject]@{
    ManifestPath = $manifestFullPath
    ReleaseTag = $ExpectedReleaseTag
    Version = $version
    SourceCommit = $ExpectedSourceCommit
    RunId = $ExpectedRunId
    RunAttempt = $ExpectedRunAttempt
    Os = $os
    Architecture = $architecture
    Target = $policy.Target
    Toolchain = $toolchain
    Job = $smoke.Runner.Job
    RunnerIdentity = $smoke.Runner.Identity
    Timestamp = [int64]$timestamp
    SubjectBytes = $subjectBytes
    SubjectDigest = $subjectDigest
    SubjectByteCount = [int64]$subjectBytes.LongLength
    EnvelopeBytes = $envelopeBytes
    EnvelopeDigest = $envelopeDigest
    EnvelopeByteCount = [int64]$envelopeBytes.LongLength
    ArchivePath = $archive.Path
    ArchiveName = $policy.Archive
    ArchiveFormat = $policy.Format
    Binary = $policy.Binary
    ArchiveByteCount = $archive.ByteCount
    ArchiveSha256 = $archive.Sha256
  }
}

Assert-TrustedInputs

if ($PSCmdlet.ParameterSetName -eq "Verify") {
  return Read-AndVerifyPlatformManifest $VerifyManifestPath
}

$manifestFullPath = Get-FullPath $ManifestPath
$root = [System.IO.Path]::GetDirectoryName($manifestFullPath)
if ([string]::IsNullOrWhiteSpace($root)) {
  Fail-PlatformEvidence "platform_manifest_path_invalid" $ManifestPath
}
[System.IO.Directory]::CreateDirectory($root) | Out-Null
$null = Assert-SafeOutputFile $root $manifestFullPath "platform manifest"
$version = $ExpectedReleaseTag.Substring(1)
$os = Normalize-Os $ExpectedOs
$architecture = Normalize-Architecture $ExpectedArchitecture
$policy = Get-PlatformPolicy $os $architecture $version
$smoke = Read-CaptureEvidence $SmokeCapturePath $root "smoke" $os $architecture
$toolchainCapture = Read-CaptureEvidence $ToolchainCapturePath $root "toolchain" $os $architecture
Assert-CaptureRunnerConsistency $smoke $toolchainCapture
Assert-SmokeVersion $smoke.Stdout.Text $version
$toolchain = Get-ToolchainLine $toolchainCapture.Stdout.Text

$archiveFullPath = Resolve-InputFileBelowRoot $root $ArchivePath "archive"
if ([System.IO.Path]::GetFileName($archiveFullPath) -cne $policy.Archive) {
  Fail-PlatformEvidence "platform_archive_name_mismatch" ([System.IO.Path]::GetFileName($archiveFullPath))
}
$archiveDigest = Get-FileDigest $archiveFullPath
$archive = [pscustomobject]@{
  Path = ConvertTo-RootRelativePath $root $archiveFullPath "archive.path"
  ByteCount = $archiveDigest.ByteCount
  Sha256 = $archiveDigest.Sha256
}

$subjectPath = Assert-SafeOutputFile $root (Join-Path $root "$([System.IO.Path]::GetFileNameWithoutExtension($manifestFullPath)).subject") "subject output"
$envelopePath = Assert-SafeOutputFile $root (Join-Path $root "$([System.IO.Path]::GetFileNameWithoutExtension($manifestFullPath)).envelope") "envelope output"
foreach ($candidate in @($manifestFullPath, $subjectPath, $envelopePath)) {
  foreach ($input in @((Get-FullPath $SmokeCapturePath), (Get-FullPath $ToolchainCapturePath), $archiveFullPath)) {
    if ($candidate -eq $input) {
      Fail-PlatformEvidence "platform_output_path_conflict" $candidate
    }
  }
}

$subjectText = New-PlatformSubject $ExpectedReleaseTag $version $ExpectedSourceCommit $ExpectedRunId $ExpectedRunAttempt $os $architecture $policy $toolchain $smoke.Runner $archive $smoke $toolchainCapture
$subjectBytes = $Utf8NoBom.GetBytes($subjectText)
$subjectDigest = Get-BytesDigest $subjectBytes
$timestamp = [System.Math]::Max($smoke.FinishedAtEpochMilliseconds, $toolchainCapture.FinishedAtEpochMilliseconds)
$envelopeText = New-EvidenceEnvelope $ExpectedSourceCommit $policy.Target $os $architecture $toolchain $smoke.Runner.Identity $timestamp $subjectDigest
Write-Utf8LfFile $subjectPath $subjectText
Write-Utf8LfFile $envelopePath $envelopeText
$envelopeIdentity = Get-FileDigest $envelopePath

$index = [ordered]@{
  format = $PlatformFormat
  release_tag = $ExpectedReleaseTag
  version = $version
  source_commit = $ExpectedSourceCommit
  run_id = $ExpectedRunId
  run_attempt = $ExpectedRunAttempt
  platform = [ordered]@{
    os = $os
    architecture = $architecture
    target = $policy.Target
    toolchain = $toolchain
  }
  archive = [ordered]@{
    path = $archive.Path
    name = $policy.Archive
    format = $policy.Format
    binary = $policy.Binary
    byte_count = $archive.ByteCount
    sha256 = $archive.Sha256
  }
  captures = [ordered]@{
    smoke = Convert-CaptureToIndex $smoke
    toolchain = Convert-CaptureToIndex $toolchainCapture
  }
  subject = [ordered]@{
    path = ConvertTo-RootRelativePath $root $subjectPath "subject.path"
    byte_count = [int64]$subjectBytes.LongLength
    sha256 = $subjectDigest
  }
  envelope = [ordered]@{
    path = ConvertTo-RootRelativePath $root $envelopePath "envelope.path"
    byte_count = $envelopeIdentity.ByteCount
    sha256 = $envelopeIdentity.Sha256
  }
}
Write-JsonLfFile $manifestFullPath $index
$null = Read-AndVerifyPlatformManifest $manifestFullPath
Write-Output $manifestFullPath
