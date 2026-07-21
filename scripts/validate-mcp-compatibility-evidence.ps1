[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$EvidencePath,

  [Parameter(Mandatory = $true)]
  [ValidatePattern('^[1-9][0-9]*$')]
  [string]$ExpectedRunId,

  [Parameter(Mandatory = $true)]
  [ValidatePattern('^[1-9][0-9]*$')]
  [string]$ExpectedRunAttempt
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$SubjectName = "release-mcp-compatibility.evidence"
$CaptureName = "mcp-compatibility.capture.json"
$StdoutName = "mcp-compatibility.stdout.json"
$StderrName = "mcp-compatibility.stderr"
$Utf8Strict = New-Object System.Text.UTF8Encoding($false, $true)
$ExpectedFields = @(
  "format",
  "evidence_kind",
  "server_name",
  "server_version",
  "protocol_version",
  "transport",
  "tls_handshake_completed",
  "tls_peer_name",
  "tls_protocol",
  "tls_handshake_count",
  "tool_name",
  "schema_sha256",
  "schema_bytes",
  "output_sha256",
  "output_bytes",
  "initialize_server_info_observed",
  "tools_list_schema_observed",
  "tools_call_result_observed",
  "abort_socket_closed",
  "abort_session_deleted",
  "abort_reader_joined",
  "abort_sessions_after",
  "abort_readers_after",
  "abort_cleanup_pending_after"
)

function Fail-Evidence {
  param([string]$Reason, [string]$Detail)

  $safeDetail = if ([string]::IsNullOrWhiteSpace($Detail)) {
    "none"
  } else {
    $Detail.Replace("`r", " ").Replace("`n", " ")
  }
  throw "[mcp-compatibility-evidence] reason=$Reason detail=$safeDetail"
}

function Assert-RegularFile {
  param([string]$Path, [string]$Reason)

  if (-not [System.IO.File]::Exists($Path)) {
    Fail-Evidence $Reason $Path
  }
  if (([System.IO.File]::GetAttributes($Path) -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
    Fail-Evidence "evidence_path_symlink" $Path
  }
}

function Get-Sha256 {
  param([string]$Path)

  Get-BytesSha256 ([System.IO.File]::ReadAllBytes($Path))
}

function Get-BytesSha256 {
  param([byte[]]$Bytes)

  $algorithm = [System.Security.Cryptography.SHA256]::Create()
  try {
    $digest = $algorithm.ComputeHash($Bytes)
    return "sha256:$([System.BitConverter]::ToString($digest).Replace('-', '').ToLowerInvariant())"
  } finally {
    $algorithm.Dispose()
  }
}

function Read-StrictUtf8 {
  param([string]$Path, [string]$Reason)

  Assert-RegularFile $Path $Reason
  try {
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    return $Utf8Strict.GetString($bytes)
  } catch {
    Fail-Evidence "evidence_utf8_invalid" $Path
  }
}

function Assert-CanonicalDigest {
  param([string]$Value, [string]$Field)

  if ($Value -cnotmatch '^sha256:[0-9a-f]{64}$') {
    Fail-Evidence "subject_field_invalid" $Field
  }
}

function Assert-PositiveDecimal {
  param([string]$Value, [string]$Field)

  if ($Value -cnotmatch '^[1-9][0-9]*$') {
    Fail-Evidence "subject_field_invalid" $Field
  }
}

function Read-CanonicalSubject {
  param([string]$Path)

  $text = Read-StrictUtf8 $Path "subject_missing"
  if ($text.Length -eq 0 -or -not $text.EndsWith("`n") -or $text.EndsWith("`n`n") -or
      $text.Contains("`r") -or $text.Contains([char]0) -or [int][char]$text[0] -eq 0xFEFF) {
    Fail-Evidence "subject_noncanonical" $Path
  }
  $lines = $text.Substring(0, $text.Length - 1).Split([char]"`n")
  if ($lines.Count -ne $ExpectedFields.Count) {
    Fail-Evidence "subject_field_count_invalid" $Path
  }

  $fields = [ordered]@{}
  for ($index = 0; $index -lt $ExpectedFields.Count; $index += 1) {
    $line = $lines[$index]
    $separator = $line.IndexOf('=')
    if ($separator -le 0) {
      Fail-Evidence "subject_line_invalid" "${Path}:$index"
    }
    $key = $line.Substring(0, $separator)
    $value = $line.Substring($separator + 1)
    if ($key -cne $ExpectedFields[$index] -or [string]::IsNullOrWhiteSpace($value) -or
        $value.Trim() -cne $value -or $value.Contains('=')) {
      Fail-Evidence "subject_field_invalid" "${Path}:$key"
    }
    $fields[$key] = $value
  }

  foreach ($expectation in @(
      @("format", "eva.mcp-compatibility.v1"),
      @("evidence_kind", "measurement"),
      @("server_name", "eva"),
      @("protocol_version", "2025-11-25"),
      @("transport", "streamable_http"),
      @("tls_handshake_completed", "true"),
      @("tls_peer_name", "127.0.0.1"),
      @("tool_name", "compat.echo"),
      @("initialize_server_info_observed", "true"),
      @("tools_list_schema_observed", "true"),
      @("tools_call_result_observed", "true"),
      @("abort_socket_closed", "true"),
      @("abort_session_deleted", "true"),
      @("abort_reader_joined", "true"),
      @("abort_sessions_after", "0"),
      @("abort_readers_after", "0"),
      @("abort_cleanup_pending_after", "0")
    )) {
    if ([string]$fields[$expectation[0]] -cne [string]$expectation[1]) {
      Fail-Evidence "subject_field_invalid" ([string]$expectation[0])
    }
  }
  if ([string]$fields["server_version"] -cnotmatch '^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?$') {
    Fail-Evidence "subject_field_invalid" "server_version"
  }
  if ([string]$fields["tls_protocol"] -cnotin @("TLSv1_2", "TLSv1_3")) {
    Fail-Evidence "subject_field_invalid" "tls_protocol"
  }
  Assert-PositiveDecimal ([string]$fields["tls_handshake_count"]) "tls_handshake_count"
  Assert-CanonicalDigest ([string]$fields["schema_sha256"]) "schema_sha256"
  Assert-CanonicalDigest ([string]$fields["output_sha256"]) "output_sha256"
  Assert-PositiveDecimal ([string]$fields["schema_bytes"]) "schema_bytes"
  Assert-PositiveDecimal ([string]$fields["output_bytes"]) "output_bytes"
  return $fields
}

function Resolve-CaptureStream {
  param($Claim, [string]$Directory, [string]$ExpectedName, [string]$Field)

  if ($null -eq $Claim -or [string]$Claim.path -cne $ExpectedName -or
      [string]$Claim.sha256 -cnotmatch '^sha256:[0-9a-f]{64}$' -or
      [long]$Claim.byte_count -lt 0) {
    Fail-Evidence "capture_stream_invalid" $Field
  }
  $path = Join-Path $Directory $ExpectedName
  Assert-RegularFile $path "capture_stream_missing"
  $size = [System.IO.FileInfo]::new($path).Length
  if ($size -ne [long]$Claim.byte_count -or (Get-Sha256 $path) -cne [string]$Claim.sha256) {
    Fail-Evidence "capture_stream_digest_mismatch" $Field
  }
  return $path
}

function Assert-Capture {
  param([string]$Directory, [string]$ExpectedOs)

  $capturePath = Join-Path $Directory $CaptureName
  $captureText = Read-StrictUtf8 $capturePath "capture_missing"
  try {
    $capture = $captureText | ConvertFrom-Json
  } catch {
    Fail-Evidence "capture_json_invalid" $capturePath
  }
  if ([string]$capture.format -cne "eva.release.command_capture.v1" -or
      [string]$capture.capture_id -cne "mcp.compatibility.measure" -or
      [string]$capture.executable -cne "cargo" -or [string]$capture.outcome -cne "success" -or
      [int]$capture.exit_code -ne 0 -or $null -ne $capture.failure_reason -or [long]$capture.duration_ms -lt 0) {
    Fail-Evidence "capture_outcome_invalid" $ExpectedOs
  }

  $argv = @($capture.argv | ForEach-Object { [string]$_ })
  $expectedArgv = @("run", "--quiet", "--", "mcp", "compatibility", "measure", "--subject-output")
  if ($argv.Count -ne 10) {
    Fail-Evidence "capture_argv_invalid" $ExpectedOs
  }
  for ($index = 0; $index -lt $expectedArgv.Count; $index += 1) {
    if ($argv[$index] -cne $expectedArgv[$index]) {
      Fail-Evidence "capture_argv_invalid" "${ExpectedOs}:$index"
    }
  }
  if ([System.IO.Path]::GetFileName($argv[7]) -cne $SubjectName -or
      $argv[8] -cne "--output" -or $argv[9] -cne "json") {
    Fail-Evidence "capture_argv_invalid" $ExpectedOs
  }

  $runner = $capture.runner
  if ($null -eq $runner -or [string]$runner.provider -cne "github-actions" -or
      [string]$runner.run_id -cne $ExpectedRunId -or
      [string]$runner.run_attempt -cne $ExpectedRunAttempt -or
      [string]$runner.job -cne "rust" -or [string]$runner.os -cne $ExpectedOs -or
      [string]::IsNullOrWhiteSpace([string]$runner.architecture)) {
    Fail-Evidence "capture_runner_invalid" $ExpectedOs
  }

  $stdoutPath = Resolve-CaptureStream $capture.stdout $Directory $StdoutName "stdout"
  $stderrPath = Resolve-CaptureStream $capture.stderr $Directory $StderrName "stderr"
  if ((Read-StrictUtf8 $stderrPath "capture_stderr_missing").Length -ne 0) {
    Fail-Evidence "capture_stderr_nonempty" $ExpectedOs
  }
  try {
    $response = (Read-StrictUtf8 $stdoutPath "capture_stdout_missing") | ConvertFrom-Json
  } catch {
    Fail-Evidence "capture_stdout_json_invalid" $ExpectedOs
  }
  $subjectPath = Join-Path $Directory $SubjectName
  Assert-RegularFile $subjectPath "subject_missing"
  $subjectDigest = Get-Sha256 $subjectPath
  if ($response.ok -ne $true -or [string]$response.command -cne "mcp.compatibility.measure" -or
      [int]$response.exit_code -ne 0 -or
      [string]$response.data.evidence_kind -cne "measurement" -or
      [string]$response.data.subject.sha256 -cne $subjectDigest -or
      $response.data.subject.written -ne $true) {
    Fail-Evidence "capture_stdout_contract_invalid" $ExpectedOs
  }
}

$root = [System.IO.Path]::GetFullPath($EvidencePath)
if (-not [System.IO.Directory]::Exists($root)) {
  Fail-Evidence "evidence_root_missing" $root
}
if (([System.IO.File]::GetAttributes($root) -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
  Fail-Evidence "evidence_root_symlink" $root
}

$rootEntries = @(Get-ChildItem -LiteralPath $root -Force)
if ($rootEntries.Count -ne 3 -or @($rootEntries | Where-Object { -not $_.PSIsContainer }).Count -ne 0) {
  Fail-Evidence "platform_directory_set_invalid" ([string]$rootEntries.Count)
}
foreach ($directory in $rootEntries) {
  if (([System.IO.File]::GetAttributes($directory.FullName) -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
    Fail-Evidence "evidence_path_symlink" $directory.FullName
  }
  $entries = @(Get-ChildItem -LiteralPath $directory.FullName -Force)
  $expectedNames = @($CaptureName, $StderrName, $StdoutName, $SubjectName) | Sort-Object
  $actualNames = @($entries | ForEach-Object { $_.Name }) | Sort-Object
  if ($entries.Count -ne 4 -or @($entries | Where-Object { $_.PSIsContainer }).Count -ne 0 -or
      ($actualNames -join "`n") -cne ($expectedNames -join "`n")) {
    Fail-Evidence "platform_file_set_invalid" $directory.FullName
  }
  foreach ($entry in $entries) {
    if (([System.IO.File]::GetAttributes($entry.FullName) -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
      Fail-Evidence "evidence_path_symlink" $entry.FullName
    }
  }
}
$expectedOperatingSystems = @("Linux", "Windows", "macOS")
$observedOperatingSystems = New-Object 'System.Collections.Generic.HashSet[string]' ([System.StringComparer]::Ordinal)
$subjectDigests = New-Object 'System.Collections.Generic.HashSet[string]' ([System.StringComparer]::Ordinal)
$stableProjectionDigests = New-Object 'System.Collections.Generic.HashSet[string]' ([System.StringComparer]::Ordinal)
$serverVersions = New-Object 'System.Collections.Generic.HashSet[string]' ([System.StringComparer]::Ordinal)
foreach ($platformDirectory in $rootEntries) {
  $directory = $platformDirectory.FullName
  $subjectPath = Join-Path $directory $SubjectName
  $captureText = Read-StrictUtf8 (Join-Path $directory $CaptureName) "capture_missing"
  try {
    $runnerOs = [string](($captureText | ConvertFrom-Json).runner.os)
  } catch {
    Fail-Evidence "capture_json_invalid" $directory
  }
  if ($expectedOperatingSystems -cnotcontains $runnerOs -or -not $observedOperatingSystems.Add($runnerOs)) {
    Fail-Evidence "platform_identity_invalid" $runnerOs
  }
  Assert-Capture $directory $runnerOs
  $fields = Read-CanonicalSubject $subjectPath
  [void]$subjectDigests.Add((Get-Sha256 $subjectPath))
  $stableProjection = @($ExpectedFields | Where-Object { $_ -cnotin @("tls_protocol", "tls_handshake_count") } | ForEach-Object {
      "$_=$([string]$fields[$_])"
    }) -join "`n"
  [void]$stableProjectionDigests.Add((Get-BytesSha256 $Utf8Strict.GetBytes("$stableProjection`n")))
  [void]$serverVersions.Add([string]$fields["server_version"])
}
if ($observedOperatingSystems.Count -ne 3 -or $stableProjectionDigests.Count -ne 1 -or $serverVersions.Count -ne 1) {
  Fail-Evidence "platform_subject_mismatch" "os=$($observedOperatingSystems.Count) stable=$($stableProjectionDigests.Count) version=$($serverVersions.Count)"
}

[pscustomobject]@{
  schema = "eva.mcp-compatibility.evidence_readback.v1"
  status = "verified"
  platform_count = 3
  run_id = $ExpectedRunId
  run_attempt = $ExpectedRunAttempt
  server_version = @($serverVersions)[0]
  stable_projection_digest = @($stableProjectionDigests)[0]
  subject_digests = @($subjectDigests | Sort-Object)
}
