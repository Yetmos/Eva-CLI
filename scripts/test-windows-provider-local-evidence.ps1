[CmdletBinding()]
param(
  [string]$RepositoryRoot = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($RepositoryRoot)) {
  $RepositoryRoot = Split-Path -Parent $PSScriptRoot
}
$RepositoryRoot = [System.IO.Path]::GetFullPath($RepositoryRoot)
$Validator = Join-Path $PSScriptRoot "validate-windows-provider-local-evidence.ps1"
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false, $true)
$SourceCommit = "0123456789abcdef0123456789abcdef01234567"
$RunId = "w3-local-contract"
$RunnerProvider = "local"
$RunnerName = "contract-windows-runner"
$RunnerIdentity = "$RunnerName/process-4242"
$RunnerArchitecture = "AMD64"

function Assert-True {
  param([bool]$Condition, [string]$Message)

  if (-not $Condition) {
    throw "[windows-provider-local-evidence-test] $Message"
  }
}

function Assert-Equal {
  param([object]$Actual, [object]$Expected, [string]$Message)

  if ([string]$Actual -cne [string]$Expected) {
    throw "[windows-provider-local-evidence-test] $Message actual='$Actual' expected='$Expected'"
  }
}

function Assert-FailsReason {
  param([scriptblock]$Action, [string]$Reason, [string]$ReceiptPath)

  try {
    & $Action
  } catch {
    if (-not $_.Exception.Message.Contains("reason=$Reason ")) {
      throw "[windows-provider-local-evidence-test] Expected reason '$Reason', got: $($_.Exception.Message)"
    }
    if (-not [string]::IsNullOrWhiteSpace($ReceiptPath) -and (Test-Path -LiteralPath $ReceiptPath)) {
      throw "[windows-provider-local-evidence-test] Failure wrote a receipt: $ReceiptPath"
    }
    return
  }
  throw "[windows-provider-local-evidence-test] Expected reason '$Reason', but validator succeeded."
}

function Get-Sha256Bytes {
  param([byte[]]$Bytes)

  $sha = [System.Security.Cryptography.SHA256]::Create()
  try {
    return "sha256:$([System.BitConverter]::ToString($sha.ComputeHash($Bytes)).Replace('-', '').ToLowerInvariant())"
  } finally {
    $sha.Dispose()
  }
}

function Get-Sha256File {
  param([string]$Path)

  return Get-Sha256Bytes ([System.IO.File]::ReadAllBytes($Path))
}

function Write-Utf8Text {
  param([string]$Path, [string]$Text)

  $parent = [System.IO.Path]::GetDirectoryName([System.IO.Path]::GetFullPath($Path))
  [System.IO.Directory]::CreateDirectory($parent) | Out-Null
  [System.IO.File]::WriteAllText($Path, $Text.Replace("`r`n", "`n").Replace("`r", "`n"), $Utf8NoBom)
}

function Write-Json {
  param([string]$Path, [object]$Value)

  $json = (($Value | ConvertTo-Json -Depth 16 -Compress).Replace("`r`n", "`n").Replace("`r", "`n"))
  Write-Utf8Text -Path $Path -Text "$json`n"
}

function Read-Json {
  param([string]$Path)

  return [System.IO.File]::ReadAllText($Path, $Utf8NoBom) | ConvertFrom-Json
}

function New-Plan {
  param([int]$Ordinal, [string]$Id, [string]$Executable, [string[]]$Argv, [string[]]$Claims, [string[]]$Markers)

  return @{
    ordinal = $Ordinal
    id = $Id
    executable = $Executable
    argv = $Argv
    claims = $Claims
    markers = $Markers
  }
}

function Get-Plans {
  $credentialScript = Join-Path $RepositoryRoot "scripts\test-credential-leak-scan.ps1"
  return @(
    (New-Plan 0 "process-boundary" "cargo" @("test", "-p", "eva-adapter", "process_backend::tests::", "--", "--test-threads=1", "--nocapture") @("pid_start_token", "windows_job", "descendant_cleanup", "pid_reuse_fence", "same_sid", "different_sid_reject", "unknown_sid_reject") @("windows_same_token_identity_spawns_inside_job_boundary", "windows_distinct_service_token_is_rejected_before_spawn", "windows_unknown_account_is_rejected_before_spawn", "12 passed; 0 failed; 3 ignored")),
    (New-Plan 1 "restart-budget" "cargo" @("test", "-p", "eva-adapter", "runtime::tests::runtime_crash_loop_never_exceeds_durable_restart_budget", "--", "--exact", "--test-threads=1", "--nocapture") @("bounded_restart_attempts", "budget_exhausted", "durable_restart_state") @("runtime_crash_loop_never_exceeds_durable_restart_budget", "1 passed; 0 failed")),
    (New-Plan 2 "provider-admission" "cargo" @("test", "-p", "eva-storage", "provider_admission::tests::", "--", "--test-threads=1", "--nocapture") @("capacity_one_winner", "crash_expiry_reclaim", "successor_fence") @("two_processes_have_one_winner_for_capacity_one", "crashed_process_reservation_is_reclaimed_only_after_expiry", "6 passed; 0 failed")),
    (New-Plan 3 "orphan-recovery" "cargo" @("test", "-p", "eva-runtime", "recovery::tests::", "--", "--test-threads=1", "--nocapture") @("orphan_cleanup", "pid_reuse_reject", "legacy_identity_reject", "generation_restart_preserved") @("recovery_interrupts_active_provider_process_and_preserves_task", "recovery_refuses_pid_reuse_without_mutating_task_or_process_record", "recovery_preserves_auto_restart_attempt_after_orphan_cleanup_and_generation_change", "23 passed; 0 failed")),
    (New-Plan 4 "daemon-recovery" "cargo" @("test", "-p", "eva-runtime", "daemon::tests::daemon_start_recovers_interrupted_provider_process_state", "--", "--exact", "--test-threads=1", "--nocapture") @("daemon_restart_cleanup", "interrupted_provider_retired") @("daemon_start_recovers_interrupted_provider_process_state", "1 passed; 0 failed")),
    (New-Plan 5 "credential-secret-zero" "powershell" @("-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", $credentialScript, "-RepositoryRoot", $RepositoryRoot) @("synthetic_canary_consumed", "stdout_stderr_redacted", "artifact_redacted", "scanner_negative_controls") @("Credential leak scan harness passed: 8 credential cases and 3 scanner negative controls."))
  )
}

function New-Runner {
  return [ordered]@{
    provider = $RunnerProvider
    identity = $RunnerIdentity
    name = $RunnerName
    os = "Windows"
    architecture = $RunnerArchitecture
    run_id = $null
    run_attempt = $null
    job = $null
  }
}

function New-Fixture {
  param([string]$Root, [string]$Name)

  $fixtureRoot = Join-Path $Root $Name
  $evidenceRoot = Join-Path $fixtureRoot "evidence"
  $receiptPath = Join-Path (Join-Path $fixtureRoot "receipt") "receipt.json"
  [System.IO.Directory]::CreateDirectory((Join-Path $evidenceRoot "captures")) | Out-Null
  $scenarios = New-Object System.Collections.Generic.List[object]
  foreach ($plan in @(Get-Plans)) {
    $directoryName = "{0:D2}-{1}" -f [int]$plan.ordinal, [string]$plan.id
    $captureRoot = Join-Path (Join-Path $evidenceRoot "captures") $directoryName
    [System.IO.Directory]::CreateDirectory($captureRoot) | Out-Null
    $stdoutPath = Join-Path $captureRoot "capture.stdout"
    $stderrPath = Join-Path $captureRoot "capture.stderr"
    Write-Utf8Text -Path $stdoutPath -Text (([string[]]$plan.markers -join "`n") + "`n")
    Write-Utf8Text -Path $stderrPath -Text "Finished test fixture`n"
    $stdoutBytes = [System.IO.File]::ReadAllBytes($stdoutPath)
    $stderrBytes = [System.IO.File]::ReadAllBytes($stderrPath)
    $capture = [ordered]@{
      format = "eva.release.command_capture.v1"
      capture_id = "w3-windows-local.$([string]$plan.id)"
      executable = [string]$plan.executable
      argv = [string[]]$plan.argv
      outcome = "success"
      started_at = "2026-07-28T00:00:00.0000000+00:00"
      finished_at = "2026-07-28T00:00:01.0000000+00:00"
      duration_ms = 1000
      exit_code = 0
      failure_reason = $null
      runner = New-Runner
      stdout = [ordered]@{ path = "capture.stdout"; byte_count = $stdoutBytes.Length; sha256 = Get-Sha256Bytes $stdoutBytes }
      stderr = [ordered]@{ path = "capture.stderr"; byte_count = $stderrBytes.Length; sha256 = Get-Sha256Bytes $stderrBytes }
    }
    Write-Json -Path (Join-Path $captureRoot "capture.json") -Value $capture
    $scenarios.Add([ordered]@{
        ordinal = [int]$plan.ordinal
        id = [string]$plan.id
        capture_path = "captures/$directoryName/capture.json"
        claims = [string[]]$plan.claims
      })
  }
  $bundle = [ordered]@{
    schema = "eva.windows.provider_multiprocess_local_evidence.v1"
    status = "completed_repository_local"
    source_commit = $SourceCommit
    run_id = $RunId
    repository_root = $RepositoryRoot
    scope = [ordered]@{
      os = "Windows"
      production = $false
      service_identity_attested = $false
      real_vault_authority = $false
      credential_material = "synthetic_canary"
    }
    runner = New-Runner
    scenarios = [object[]]$scenarios.ToArray()
    written_at = "2026-07-28T00:00:02.0000000+00:00"
  }
  $bundlePath = Join-Path $evidenceRoot "bundle.json"
  Write-Json -Path $bundlePath -Value $bundle
  Write-Utf8Text -Path (Join-Path $evidenceRoot "bundle.sha256") -Text "$(Get-Sha256File $bundlePath)`n"
  return [pscustomobject]@{
    Root = $fixtureRoot
    Evidence = $evidenceRoot
    Receipt = $receiptPath
  }
}

function Update-BundleDigest {
  param([string]$Evidence)

  $bundlePath = Join-Path $Evidence "bundle.json"
  Write-Utf8Text -Path (Join-Path $Evidence "bundle.sha256") -Text "$(Get-Sha256File $bundlePath)`n"
}

function Update-CaptureStreams {
  param([string]$CapturePath)

  $capture = Read-Json $CapturePath
  $directory = [System.IO.Path]::GetDirectoryName($CapturePath)
  foreach ($name in @("stdout", "stderr")) {
    $streamPath = Join-Path $directory ([string]$capture.$name.path)
    $bytes = [System.IO.File]::ReadAllBytes($streamPath)
    $capture.$name.byte_count = $bytes.Length
    $capture.$name.sha256 = Get-Sha256Bytes $bytes
  }
  Write-Json -Path $CapturePath -Value $capture
}

function Invoke-Validator {
  param([string]$EvidencePath, [string]$ReceiptPath)

  & $Validator `
    -EvidencePath $EvidencePath `
    -ExpectedSourceCommit $SourceCommit `
    -ExpectedRunId $RunId `
    -ExpectedRepositoryRoot $RepositoryRoot `
    -ExpectedRunnerProvider $RunnerProvider `
    -ExpectedRunnerIdentity $RunnerIdentity `
    -ExpectedRunnerName $RunnerName `
    -ExpectedRunnerArchitecture $RunnerArchitecture `
    -ReceiptPath $ReceiptPath | Out-Null
}

if (-not (Test-Path -LiteralPath $Validator -PathType Leaf)) {
  throw "[windows-provider-local-evidence-test] Validator is missing: $Validator"
}

$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) "eva-windows-provider-local-evidence-$([System.Guid]::NewGuid().ToString('N'))"
[System.IO.Directory]::CreateDirectory($testRoot) | Out-Null

try {
  $valid = New-Fixture -Root $testRoot -Name "valid"
  Invoke-Validator -EvidencePath $valid.Evidence -ReceiptPath $valid.Receipt
  Assert-True (Test-Path -LiteralPath $valid.Receipt -PathType Leaf) "Valid fixture did not write receipt."
  $receipt = Read-Json $valid.Receipt
  Assert-Equal ([string]$receipt.schema) "eva.windows.provider_multiprocess_local_readback_receipt.v1" "Receipt schema changed."
  Assert-Equal ([string]$receipt.status) "verified_repository_local_readback" "Receipt status changed."
  Assert-Equal ([int]$receipt.scenario_count) 6 "Receipt scenario count changed."
  Assert-Equal ([bool]$receipt.production) $false "Receipt must not claim production."

  $production = New-Fixture -Root $testRoot -Name "production"
  $productionBundlePath = Join-Path $production.Evidence "bundle.json"
  $productionBundle = Read-Json $productionBundlePath
  $productionBundle.scope.production = $true
  Write-Json -Path $productionBundlePath -Value $productionBundle
  Update-BundleDigest -Evidence $production.Evidence
  Assert-FailsReason { Invoke-Validator -EvidencePath $production.Evidence -ReceiptPath $production.Receipt } "scope_invalid" $production.Receipt

  $productionStatus = New-Fixture -Root $testRoot -Name "production-status"
  $productionStatusPath = Join-Path $productionStatus.Evidence "bundle.json"
  $productionStatusBundle = Read-Json $productionStatusPath
  $productionStatusBundle.status = "production_verified"
  Write-Json -Path $productionStatusPath -Value $productionStatusBundle
  Update-BundleDigest -Evidence $productionStatus.Evidence
  Assert-FailsReason { Invoke-Validator -EvidencePath $productionStatus.Evidence -ReceiptPath $productionStatus.Receipt } "bundle_invalid" $productionStatus.Receipt

  $digestTamper = New-Fixture -Root $testRoot -Name "digest-tamper"
  [System.IO.File]::AppendAllText((Join-Path $digestTamper.Evidence "bundle.json"), " ", $Utf8NoBom)
  Assert-FailsReason { Invoke-Validator -EvidencePath $digestTamper.Evidence -ReceiptPath $digestTamper.Receipt } "bundle_digest_mismatch" $digestTamper.Receipt

  $runnerMismatch = New-Fixture -Root $testRoot -Name "runner-mismatch"
  $runnerCapturePath = Join-Path $runnerMismatch.Evidence "captures\00-process-boundary\capture.json"
  $runnerCapture = Read-Json $runnerCapturePath
  $runnerCapture.runner.identity = "forged/process-1"
  Write-Json -Path $runnerCapturePath -Value $runnerCapture
  Assert-FailsReason { Invoke-Validator -EvidencePath $runnerMismatch.Evidence -ReceiptPath $runnerMismatch.Receipt } "capture_runner_invalid" $runnerMismatch.Receipt

  $argvMismatch = New-Fixture -Root $testRoot -Name "argv-mismatch"
  $argvCapturePath = Join-Path $argvMismatch.Evidence "captures\01-restart-budget\capture.json"
  $argvCapture = Read-Json $argvCapturePath
  $argvCapture.argv[0] = "run"
  Write-Json -Path $argvCapturePath -Value $argvCapture
  Assert-FailsReason { Invoke-Validator -EvidencePath $argvMismatch.Evidence -ReceiptPath $argvMismatch.Receipt } "capture_invalid" $argvMismatch.Receipt

  $markerMissing = New-Fixture -Root $testRoot -Name "marker-missing"
  $markerCapturePath = Join-Path $markerMissing.Evidence "captures\02-provider-admission\capture.json"
  Write-Utf8Text -Path (Join-Path ([System.IO.Path]::GetDirectoryName($markerCapturePath)) "capture.stdout") -Text "6 passed; 0 failed`n"
  Update-CaptureStreams -CapturePath $markerCapturePath
  Assert-FailsReason { Invoke-Validator -EvidencePath $markerMissing.Evidence -ReceiptPath $markerMissing.Receipt } "scenario_marker_missing" $markerMissing.Receipt

  $streamTamper = New-Fixture -Root $testRoot -Name "stream-tamper"
  [System.IO.File]::AppendAllText((Join-Path $streamTamper.Evidence "captures\03-orphan-recovery\capture.stdout"), "tamper", $Utf8NoBom)
  Assert-FailsReason { Invoke-Validator -EvidencePath $streamTamper.Evidence -ReceiptPath $streamTamper.Receipt } "capture_stream_invalid" $streamTamper.Receipt

  $extraFile = New-Fixture -Root $testRoot -Name "extra-file"
  Write-Utf8Text -Path (Join-Path $extraFile.Evidence "unexpected.txt") -Text "unexpected`n"
  Assert-FailsReason { Invoke-Validator -EvidencePath $extraFile.Evidence -ReceiptPath $extraFile.Receipt } "file_set_invalid" $extraFile.Receipt

  $receiptInside = New-Fixture -Root $testRoot -Name "receipt-inside"
  $insideReceipt = Join-Path $receiptInside.Evidence "receipt.json"
  Assert-FailsReason { Invoke-Validator -EvidencePath $receiptInside.Evidence -ReceiptPath $insideReceipt } "receipt_inside_evidence" $insideReceipt

  $receiptExists = New-Fixture -Root $testRoot -Name "receipt-exists"
  Write-Utf8Text -Path $receiptExists.Receipt -Text "{}"
  Assert-FailsReason { Invoke-Validator -EvidencePath $receiptExists.Evidence -ReceiptPath $receiptExists.Receipt } "receipt_exists" ""

  if ($env:OS -eq "Windows_NT") {
    $reparse = New-Fixture -Root $testRoot -Name "reparse"
    $target = Join-Path $testRoot "reparse-target"
    [System.IO.Directory]::CreateDirectory($target) | Out-Null
    $link = Join-Path $reparse.Evidence "linked"
    try {
      New-Item -ItemType Junction -Path $link -Target $target -ErrorAction Stop | Out-Null
      Assert-FailsReason { Invoke-Validator -EvidencePath $reparse.Evidence -ReceiptPath $reparse.Receipt } "path_reparse_point" $reparse.Receipt
    } catch {
      if ($_.Exception.Message.Contains("[windows-provider-local-evidence-test]") -or $_.Exception.Message.Contains("[windows-provider-local-readback]")) {
        throw
      }
    } finally {
      if (Test-Path -LiteralPath $link) {
        [System.IO.Directory]::Delete($link)
      }
    }
  }

  Write-Host "Windows provider local evidence validator self-test passed: valid local receipt and strict negative contract."
} finally {
  if (Test-Path -LiteralPath $testRoot) {
    Remove-Item -LiteralPath $testRoot -Recurse -Force
  }
}
