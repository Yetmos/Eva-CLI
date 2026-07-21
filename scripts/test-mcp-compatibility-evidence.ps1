[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$Validator = Join-Path $PSScriptRoot "validate-mcp-compatibility-evidence.ps1"
$RunId = "123456"
$RunAttempt = "2"

function Get-Sha256 {
  param([string]$Path)
  $algorithm = [System.Security.Cryptography.SHA256]::Create()
  try {
    $digest = $algorithm.ComputeHash([System.IO.File]::ReadAllBytes($Path))
    "sha256:$([System.BitConverter]::ToString($digest).Replace('-', '').ToLowerInvariant())"
  } finally {
    $algorithm.Dispose()
  }
}

function Write-Text {
  param([string]$Path, [string]$Text)
  [System.IO.Directory]::CreateDirectory([System.IO.Path]::GetDirectoryName($Path)) | Out-Null
  [System.IO.File]::WriteAllText($Path, $Text, $Utf8NoBom)
}

function Write-PlatformFixture {
  param(
    [string]$Root,
    [string]$OperatingSystem,
    [string]$TlsHandshakeCount = "1"
  )

  $directory = Join-Path $Root $OperatingSystem.ToLowerInvariant()
  [System.IO.Directory]::CreateDirectory($directory) | Out-Null
  $subjectPath = Join-Path $directory "release-mcp-compatibility.evidence"
  $subject = @(
    "format=eva.mcp-compatibility.v1"
    "evidence_kind=measurement"
    "server_name=eva"
    "server_version=1.11.5-alpha"
    "protocol_version=2025-11-25"
    "transport=streamable_http"
    "tls_handshake_completed=true"
    "tls_peer_name=127.0.0.1"
    "tls_protocol=TLSv1_3"
    "tls_handshake_count=$TlsHandshakeCount"
    "tool_name=compat.echo"
    "schema_sha256=sha256:$('a' * 64)"
    "schema_bytes=128"
    "output_sha256=sha256:$('b' * 64)"
    "output_bytes=16"
    "initialize_server_info_observed=true"
    "tools_list_schema_observed=true"
    "tools_call_result_observed=true"
    "abort_socket_closed=true"
    "abort_session_deleted=true"
    "abort_reader_joined=true"
    "abort_sessions_after=0"
    "abort_readers_after=0"
    "abort_cleanup_pending_after=0"
  ) -join "`n"
  Write-Text $subjectPath "$subject`n"

  $stdoutPath = Join-Path $directory "mcp-compatibility.stdout.json"
  $stderrPath = Join-Path $directory "mcp-compatibility.stderr"
  $response = [ordered]@{
    ok = $true
    command = "mcp.compatibility.measure"
    exit_code = 0
    data = [ordered]@{
      evidence_kind = "measurement"
      subject = [ordered]@{
        sha256 = Get-Sha256 $subjectPath
        bytes = [System.IO.FileInfo]::new($subjectPath).Length
        written = $true
      }
    }
    trace = [ordered]@{ span_id = "cli.mcp.compatibility.measure" }
  }
  Write-Text $stdoutPath (($response | ConvertTo-Json -Depth 6 -Compress) + "`n")
  Write-Text $stderrPath ""
  $capturedSubjectPath = switch ($OperatingSystem) {
    "Windows" { "D:\a\Eva-CLI\Eva-CLI\.eva\w4-mcp-compatibility-evidence\release-mcp-compatibility.evidence" }
    "macOS" { "/Users/runner/work/Eva-CLI/Eva-CLI/.eva/w4-mcp-compatibility-evidence/release-mcp-compatibility.evidence" }
    default { "/home/runner/work/Eva-CLI/Eva-CLI/.eva/w4-mcp-compatibility-evidence/release-mcp-compatibility.evidence" }
  }
  $capture = [ordered]@{
    format = "eva.release.command_capture.v1"
    capture_id = "mcp.compatibility.measure"
    executable = "cargo"
    argv = @("run", "--quiet", "--", "mcp", "compatibility", "measure", "--subject-output", $capturedSubjectPath, "--output", "json")
    outcome = "success"
    started_at = "2026-01-01T00:00:00.0000000+00:00"
    finished_at = "2026-01-01T00:00:01.0000000+00:00"
    duration_ms = 1000
    exit_code = 0
    failure_reason = $null
    runner = [ordered]@{
      provider = "github-actions"
      identity = "contract/$RunId/$RunAttempt/rust"
      name = "contract"
      os = $OperatingSystem
      architecture = "X64"
      run_id = $RunId
      run_attempt = $RunAttempt
      job = "rust"
    }
    stdout = [ordered]@{ path = "mcp-compatibility.stdout.json"; byte_count = [System.IO.FileInfo]::new($stdoutPath).Length; sha256 = Get-Sha256 $stdoutPath }
    stderr = [ordered]@{ path = "mcp-compatibility.stderr"; byte_count = 0; sha256 = Get-Sha256 $stderrPath }
  }
  Write-Text (Join-Path $directory "mcp-compatibility.capture.json") (($capture | ConvertTo-Json -Depth 8) + "`n")
}

function Assert-Fails {
  param([scriptblock]$Action, [string]$ExpectedReason)
  $message = $null
  try {
    & $Action
  } catch {
    $message = $_.Exception.Message
  }
  if ([string]::IsNullOrWhiteSpace($message) -or -not $message.Contains("reason=$ExpectedReason")) {
    throw "Expected failure '$ExpectedReason', got '$message'."
  }
}

$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) "eva-mcp-compatibility-evidence-$([guid]::NewGuid().ToString('N'))"
try {
  foreach ($operatingSystem in @("Linux", "Windows", "macOS")) {
    Write-PlatformFixture $tempRoot $operatingSystem
  }
  $result = & $Validator -EvidencePath $tempRoot -ExpectedRunId $RunId -ExpectedRunAttempt $RunAttempt
  if ($result.status -cne "verified" -or [int]$result.platform_count -ne 3) {
    throw "Verified MCP compatibility fixture returned an invalid receipt."
  }

  $windowsCapturePath = Join-Path $tempRoot "windows/mcp-compatibility.capture.json"
  $windowsCapture = [System.IO.File]::ReadAllText($windowsCapturePath, $Utf8NoBom) | ConvertFrom-Json
  $windowsCapture.argv[7] = "D:\a\Eva-CLI\Eva-CLI\.eva\wrong-subject.evidence"
  Write-Text $windowsCapturePath (($windowsCapture | ConvertTo-Json -Depth 8) + "`n")
  Assert-Fails {
    & $Validator -EvidencePath $tempRoot -ExpectedRunId $RunId -ExpectedRunAttempt $RunAttempt | Out-Null
  } "capture_argv_invalid"
  Write-PlatformFixture $tempRoot "Windows"

  Write-Text (Join-Path $tempRoot "linux/unverified.txt") "unverified`n"
  Assert-Fails {
    & $Validator -EvidencePath $tempRoot -ExpectedRunId $RunId -ExpectedRunAttempt $RunAttempt | Out-Null
  } "platform_file_set_invalid"
  Remove-Item -LiteralPath (Join-Path $tempRoot "linux/unverified.txt") -Force

  Remove-Item -LiteralPath (Join-Path $tempRoot "windows/mcp-compatibility.capture.json") -Force
  Assert-Fails {
    & $Validator -EvidencePath $tempRoot -ExpectedRunId $RunId -ExpectedRunAttempt $RunAttempt | Out-Null
  } "platform_file_set_invalid"
  Write-PlatformFixture $tempRoot "Windows"

  Write-PlatformFixture $tempRoot "Windows" "0"
  Assert-Fails {
    & $Validator -EvidencePath $tempRoot -ExpectedRunId $RunId -ExpectedRunAttempt $RunAttempt | Out-Null
  } "subject_field_invalid"
  Write-PlatformFixture $tempRoot "Windows"

  Write-Text (Join-Path $tempRoot "windows/release-mcp-compatibility.evidence") "format=eva.mcp-compatibility.v1`n"
  Assert-Fails {
    & $Validator -EvidencePath $tempRoot -ExpectedRunId $RunId -ExpectedRunAttempt $RunAttempt | Out-Null
  } "capture_stdout_contract_invalid"

  Write-Host "MCP compatibility evidence contract passed: three-platform readback and subject tamper rejection."
} finally {
  if ([System.IO.Directory]::Exists($tempRoot)) {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force
  }
}
