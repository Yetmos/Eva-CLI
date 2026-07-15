[CmdletBinding()]
param(
  [switch]$Child,
  [switch]$Grandchild,
  [ValidateSet("success", "failure", "timeout")]
  [string]$Mode,
  [string]$Payload
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ($Grandchild) {
  Start-Sleep -Seconds 30
  exit 0
}

if ($Child) {
  [Console]::Out.WriteLine("stdout:$Payload")
  [Console]::Error.WriteLine("stderr:$Payload")
  [Console]::Out.Flush()
  [Console]::Error.Flush()
  if ($Mode -eq "failure") {
    exit 7
  }
  if ($Mode -eq "timeout") {
    $childHostExecutable = [string](Get-Process -Id $PID).Path
    if ([string]::IsNullOrWhiteSpace($childHostExecutable)) {
      $childHostName = if ($env:OS -eq "Windows_NT") { "powershell.exe" } else { "pwsh" }
      $childHostExecutable = Join-Path $PSHOME $childHostName
    }
    $startParameters = @{
      FilePath = $childHostExecutable
      ArgumentList = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $PSCommandPath, "-Grandchild")
      PassThru = $true
    }
    if ($env:OS -eq "Windows_NT") {
      $startParameters.WindowStyle = "Hidden"
    }
    $grandchildProcess = Start-Process @startParameters
    [Console]::Out.WriteLine("grandchild_pid=$($grandchildProcess.Id)")
    [Console]::Out.Flush()
    Start-Sleep -Seconds 30
  }
  exit 0
}

function Assert-Equal {
  param($Actual, $Expected, [string]$Message)
  if ($Actual -ne $Expected) {
    throw "$Message (expected '$Expected', got '$Actual')"
  }
}

function Assert-True {
  param([bool]$Condition, [string]$Message)
  if (-not $Condition) {
    throw $Message
  }
}

function Get-Sha256 {
  param([string]$Path)
  $hash = Get-FileHash -Algorithm SHA256 -LiteralPath $Path
  return "sha256:$($hash.Hash.ToLowerInvariant())"
}

function Read-Capture {
  param([string]$Path)
  return Get-Content -Raw -Encoding utf8 -LiteralPath $Path | ConvertFrom-Json
}

function Assert-StreamEvidence {
  param(
    $Stream,
    [string]$ManifestDirectory,
    [string]$ExpectedContent,
    [string]$Label,
    [switch]$StartsWith
  )

  Assert-True (-not [System.IO.Path]::IsPathRooted([string]$Stream.path)) "$Label path must be relative."
  $path = Join-Path $ManifestDirectory ([string]$Stream.path)
  Assert-True (Test-Path -LiteralPath $path -PathType Leaf) "$Label output file is missing."
  $content = [System.IO.File]::ReadAllText($path, (New-Object System.Text.UTF8Encoding($false)))
  if ($StartsWith) {
    Assert-True $content.StartsWith($ExpectedContent) "$Label content prefix changed during capture."
  } else {
    Assert-Equal $content $ExpectedContent "$Label content changed during capture."
  }
  Assert-Equal ([int64](Get-Item -LiteralPath $path).Length) ([int64]$Stream.byte_count) "$Label byte count is not reproducible."
  Assert-Equal (Get-Sha256 $path) ([string]$Stream.sha256) "$Label SHA-256 is not reproducible."
}

function Assert-GrandchildStopped {
  param(
    $Capture,
    [string]$ManifestDirectory,
    [System.Collections.Generic.List[int]]$ObservedProcessIds
  )

  $stdoutPath = Join-Path $ManifestDirectory ([string]$Capture.stdout.path)
  $stdout = [System.IO.File]::ReadAllText($stdoutPath, (New-Object System.Text.UTF8Encoding($false)))
  $match = [regex]::Match($stdout, "(?m)^grandchild_pid=(\d+)\r?$")
  Assert-True $match.Success "Timeout fixture did not report its grandchild PID."
  $grandchildPid = [int]$match.Groups[1].Value
  $ObservedProcessIds.Add($grandchildPid)

  $stopped = $false
  for ($attempt = 0; $attempt -lt 20; $attempt += 1) {
    if ($null -eq (Get-Process -Id $grandchildPid -ErrorAction SilentlyContinue)) {
      $stopped = $true
      break
    }
    Start-Sleep -Milliseconds 100
  }
  Assert-True $stopped "Timeout left grandchild process $grandchildPid running."
}

$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$captureScript = Join-Path $PSScriptRoot "capture-release-evidence.ps1"
$hostExecutable = [string](Get-Process -Id $PID).Path
if ([string]::IsNullOrWhiteSpace($hostExecutable)) {
  $hostName = if ($env:OS -eq "Windows_NT") { "powershell.exe" } else { "pwsh" }
  $hostExecutable = Join-Path $PSHOME $hostName
}
$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) "eva-release-capture-$([System.Guid]::NewGuid().ToString('N'))"
[System.IO.Directory]::CreateDirectory($testRoot) | Out-Null
$observedGrandchildPids = New-Object "System.Collections.Generic.List[int]"

try {
  $ciWorkflow = Get-Content -Raw -Encoding utf8 -LiteralPath (Join-Path $root ".github/workflows/ci.yml")
  $releaseWorkflow = Get-Content -Raw -Encoding utf8 -LiteralPath (Join-Path $root ".github/workflows/release.yml")
  Assert-True $ciWorkflow.Contains("./scripts/test-capture-release-evidence.ps1") "CI must run the capture contract."
  Assert-True $releaseWorkflow.Contains("./scripts/capture-release-evidence.ps1") "Release workflow must use the capture script."
  Assert-True (-not [regex]::IsMatch($releaseWorkflow, "(?m)^\s*Measure-Command\s*\{")) "Release workflow must not use Measure-Command for evidence."
  Assert-True (-not [regex]::IsMatch($releaseWorkflow, "(?m)^\s*cargo run .*\|\s*Tee-Object")) "Release workflow gate commands must use capture manifests."

  $payload = 'value with spaces, "quotes", and $(Write-Output injected)\'
  $cases = @(
    [ordered]@{ mode = "success"; timeout = 5000; outcome = "success"; exit_code = 0 },
    [ordered]@{ mode = "failure"; timeout = 5000; outcome = "failure"; exit_code = 7 },
    [ordered]@{ mode = "timeout"; timeout = 3000; outcome = "timeout"; exit_code = $null }
  )

  foreach ($case in $cases) {
    $caseDirectory = Join-Path $testRoot $case.mode
    $manifestPath = Join-Path $caseDirectory "capture.json"
    $arguments = @(
      "-NoProfile",
      "-ExecutionPolicy", "Bypass",
      "-File", $PSCommandPath,
      "-Child",
      "-Mode", $case.mode,
      "-Payload", $payload
    )

    $null = & $captureScript `
      -Executable $hostExecutable `
      -ArgumentList $arguments `
      -ManifestPath $manifestPath `
      -TimeoutMilliseconds $case.timeout `
      -CaptureId "test.$($case.mode)" `
      -NoFail

    $capture = Read-Capture $manifestPath
    Assert-Equal ([string]$capture.format) "eva.release.command_capture.v1" "Capture format changed."
    Assert-Equal ([string]$capture.capture_id) "test.$($case.mode)" "Capture ID changed."
    Assert-Equal ([string]$capture.outcome) $case.outcome "Outcome did not represent $($case.mode)."
    if ($null -eq $case.exit_code) {
      Assert-True ($null -eq $capture.exit_code) "Timeout must not invent an exit code."
    } else {
      Assert-Equal ([int]$capture.exit_code) ([int]$case.exit_code) "Exit code did not represent $($case.mode)."
    }
    if ($case.mode -eq "success") {
      Assert-True ($null -eq $capture.failure_reason) "Success must not invent a failure reason."
    } else {
      Assert-True (-not [string]::IsNullOrWhiteSpace([string]$capture.failure_reason)) "$($case.mode) failure reason is missing."
    }
    Assert-True ([int64]$capture.duration_ms -ge 0) "Duration must be non-negative."
    Assert-True ([System.DateTimeOffset]::Parse([string]$capture.finished_at) -ge [System.DateTimeOffset]::Parse([string]$capture.started_at)) "Capture timestamps are reversed."
    Assert-Equal @($capture.argv).Count $arguments.Count "argv length changed."
    for ($index = 0; $index -lt $arguments.Count; $index += 1) {
      Assert-Equal ([string]@($capture.argv)[$index]) $arguments[$index] "argv[$index] changed."
    }
    Assert-True (-not [string]::IsNullOrWhiteSpace([string]$capture.runner.identity)) "Runner identity is missing."
    Assert-True (-not [string]::IsNullOrWhiteSpace([string]$capture.runner.os)) "Runner OS is missing."
    Assert-True (-not [string]::IsNullOrWhiteSpace([string]$capture.runner.architecture)) "Runner architecture is missing."

    if ($case.mode -eq "timeout") {
      Assert-StreamEvidence $capture.stdout $caseDirectory "stdout:$payload$([System.Environment]::NewLine)" "stdout" -StartsWith
      Assert-GrandchildStopped $capture $caseDirectory $observedGrandchildPids
    } else {
      Assert-StreamEvidence $capture.stdout $caseDirectory "stdout:$payload$([System.Environment]::NewLine)" "stdout"
    }
    Assert-StreamEvidence $capture.stderr $caseDirectory "stderr:$payload$([System.Environment]::NewLine)" "stderr"
  }

  foreach ($mode in @("failure")) {
    $case = $cases | Where-Object { $_.mode -eq $mode }
    $caseDirectory = Join-Path $testRoot "propagate-$mode"
    $manifestPath = Join-Path $caseDirectory "capture.json"
    $arguments = @(
      "-NoProfile",
      "-ExecutionPolicy", "Bypass",
      "-File", $PSCommandPath,
      "-Child",
      "-Mode", $mode,
      "-Payload", $payload
    )
    $didThrow = $false
    try {
      $null = & $captureScript `
        -Executable $hostExecutable `
        -ArgumentList $arguments `
        -ManifestPath $manifestPath `
        -TimeoutMilliseconds $case.timeout `
        -CaptureId "test.propagate.$mode"
    } catch {
      $didThrow = $true
      Assert-True $_.Exception.Message.Contains("[release-evidence-capture] outcome=$($case.outcome)") "$mode propagation error is unstable."
    }
    Assert-True $didThrow "$mode must propagate after persisting its capture."
    $capture = Read-Capture $manifestPath
    Assert-Equal ([string]$capture.outcome) $case.outcome "$mode propagation changed the persisted outcome."
  }

  $timeoutCapture = Read-Capture (Join-Path $testRoot "timeout/capture.json")
  Assert-True ([int64]$timeoutCapture.duration_ms -ge 2500) "Timeout returned before its configured deadline."
  Assert-True ([int64]$timeoutCapture.duration_ms -lt 15000) "Timeout did not terminate the process tree promptly."

  Write-Host "Release evidence capture contract passed: success, failure, timeout, argv, and stream digests."
} finally {
  foreach ($processId in $observedGrandchildPids) {
    Get-Process -Id $processId -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
  }
  if (Test-Path -LiteralPath $testRoot) {
    Remove-Item -LiteralPath $testRoot -Recurse -Force
  }
}
