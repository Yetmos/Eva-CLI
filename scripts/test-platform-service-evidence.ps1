[CmdletBinding()]
param(
  [string]$RepositoryRoot = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($RepositoryRoot)) {
  $RepositoryRoot = Split-Path -Parent $PSScriptRoot
}

$Utf8NoBom = New-Object System.Text.UTF8Encoding($false, $true)
$Validator = Join-Path $PSScriptRoot "validate-platform-service-evidence.ps1"
$SourceCommit = "0123456789abcdef0123456789abcdef01234567"
$RunId = "ext01-readback"
$RunnerProvider = "github-actions"
$RunnerRunId = "9001"
$RunnerRunAttempt = "1"
$RunnerJob = "platform-service"
$RunnerName = "trusted-windows-runner"
$RunnerIdentity = "$RunnerName/$RunnerRunId/$RunnerRunAttempt/$RunnerJob"
$EvaDigest = "sha256:$('a' * 64)"

function Assert-True {
  param(
    [bool]$Condition,
    [string]$Message
  )

  if (-not $Condition) {
    throw "[platform-service-evidence-test] $Message"
  }
}

function Assert-Equal {
  param(
    [object]$Actual,
    [object]$Expected,
    [string]$Message
  )

  if ([string]$Actual -cne [string]$Expected) {
    throw "[platform-service-evidence-test] $Message actual='$Actual' expected='$Expected'"
  }
}

function Assert-FailsReason {
  param(
    [scriptblock]$Action,
    [string]$Reason,
    [string]$ReceiptPath
  )

  try {
    & $Action
  } catch {
    $message = $_.Exception.ToString()
    Assert-True $message.Contains("reason=$Reason") "Expected reason '$Reason', got: $message"
    if (-not [string]::IsNullOrWhiteSpace($ReceiptPath)) {
      Assert-True (-not (Test-Path -LiteralPath $ReceiptPath)) "Failure must not leave receipt '$ReceiptPath'."
    }
    return
  }
  throw "[platform-service-evidence-test] Expected reason '$Reason', but validator succeeded."
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
  $normalized = $Text.Replace("`r`n", "`n").Replace("`r", "`n").TrimEnd([char[]]@("`n")) + "`n"
  [System.IO.File]::WriteAllText($Path, $normalized, $Utf8NoBom)
}

function Write-Json {
  param(
    [string]$Path,
    [object]$Value
  )

  Write-Utf8LfFile -Path $Path -Text ($Value | ConvertTo-Json -Depth 16)
}

function Read-Json {
  param([string]$Path)

  $text = [System.IO.File]::ReadAllText($Path, $Utf8NoBom)
  $convertFromJson = Get-Command ConvertFrom-Json -ErrorAction Stop
  if ($convertFromJson.Parameters.ContainsKey("DateKind")) {
    return $text | ConvertFrom-Json -DateKind String
  }
  return $text | ConvertFrom-Json
}

function Get-Sha256Bytes {
  param([byte[]]$Bytes)

  $sha256 = [System.Security.Cryptography.SHA256]::Create()
  try {
    $digest = $sha256.ComputeHash($Bytes)
    return "sha256:$([System.BitConverter]::ToString($digest).Replace('-', '').ToLowerInvariant())"
  } finally {
    $sha256.Dispose()
  }
}

function Get-Sha256File {
  param([string]$Path)

  return Get-Sha256Bytes ([System.IO.File]::ReadAllBytes($Path))
}

function Write-DigestSidecar {
  param([string]$JsonPath)

  $digestPath = [System.IO.Path]::ChangeExtension($JsonPath, ".sha256")
  Write-Utf8LfFile -Path $digestPath -Text (Get-Sha256File $JsonPath)
}

function Get-LifecyclePlan {
  return @(
    @{ ordinal = 0; step_id = "status-preflight"; command = "status"; state = "not_installed"; mutation = $false },
    @{ ordinal = 1; step_id = "install"; command = "install"; state = "stopped"; mutation = $true },
    @{ ordinal = 2; step_id = "install-idempotent"; command = "install"; state = "stopped"; mutation = $false },
    @{ ordinal = 3; step_id = "start"; command = "start"; state = "running"; mutation = $true },
    @{ ordinal = 4; step_id = "start-idempotent"; command = "start"; state = "running"; mutation = $false },
    @{ ordinal = 5; step_id = "status-running"; command = "status"; state = "running"; mutation = $false },
    @{ ordinal = 6; step_id = "restart"; command = "restart"; state = "running"; mutation = $true },
    @{ ordinal = 7; step_id = "status-post-restart"; command = "status"; state = "running"; mutation = $false },
    @{ ordinal = 8; step_id = "stop"; command = "stop"; state = "stopped"; mutation = $true },
    @{ ordinal = 9; step_id = "stop-idempotent"; command = "stop"; state = "stopped"; mutation = $false },
    @{ ordinal = 10; step_id = "uninstall"; command = "uninstall"; state = "not_installed"; mutation = $true },
    @{ ordinal = 11; step_id = "uninstall-idempotent"; command = "uninstall"; state = "not_installed"; mutation = $false },
    @{ ordinal = 12; step_id = "status-final"; command = "status"; state = "not_installed"; mutation = $false }
  )
}

function New-PlatformServiceFixture {
  param(
    [string]$Root,
    [string]$Name
  )

  $fixtureRoot = Join-Path $Root $Name
  $evidenceRoot = Join-Path $fixtureRoot "evidence"
  $receiptPath = Join-Path (Join-Path $fixtureRoot "receipt") "receipt.json"
  $projectRoot = Join-Path $fixtureRoot "project"
  $serviceName = "eva-ext01-$RunId"
  [System.IO.Directory]::CreateDirectory($evidenceRoot) | Out-Null

  $owner = [ordered]@{
    format = "eva.windows.platform_service_evidence.v1"
    source_commit = $SourceCommit
    run_id = $RunId
    service_name = $serviceName
    repository_root = [System.IO.Path]::GetFullPath($RepositoryRoot)
    project_root = [System.IO.Path]::GetFullPath($projectRoot)
    eva_executable_sha256 = $EvaDigest
  }
  Write-Json -Path (Join-Path $evidenceRoot "platform-service-harness.owner.json") -Value $owner

  $steps = New-Object System.Collections.Generic.List[object]
  foreach ($plan in @(Get-LifecyclePlan)) {
    $captureDirectory = Join-Path (Join-Path $evidenceRoot "captures") ("{0:D2}-{1}" -f [int]$plan.ordinal, [string]$plan.step_id)
    [System.IO.Directory]::CreateDirectory($captureDirectory) | Out-Null
    $stdoutPath = Join-Path $captureDirectory "capture.stdout"
    $stderrPath = Join-Path $captureDirectory "capture.stderr"
    $stdout = [ordered]@{
      ok = $true
      exit_code = 0
      command = "service.$($plan.command)"
      data = [ordered]@{
        kind = "windows_service"
        service_name = $serviceName
        production_adapter = $true
        state = $plan.state
        mutation_executed = [bool]$plan.mutation
      }
    }
    Write-Utf8LfFile -Path $stdoutPath -Text ($stdout | ConvertTo-Json -Depth 8 -Compress)
    [System.IO.File]::WriteAllText($stderrPath, "", $Utf8NoBom)
    $capture = [ordered]@{
      format = "eva.release.command_capture.v1"
      capture_id = "platform-service.$($plan.step_id)"
      executable = "C:\eva\eva.exe"
      argv = @("service", [string]$plan.command, "--project", [System.IO.Path]::GetFullPath($projectRoot), "--output", "json")
      outcome = "success"
      started_at = "2026-07-27T00:00:00.0000000+00:00"
      finished_at = "2026-07-27T00:00:01.0000000+00:00"
      duration_ms = 1000
      exit_code = 0
      failure_reason = $null
      runner = [ordered]@{
        provider = $RunnerProvider
        identity = $RunnerIdentity
        name = $RunnerName
        os = "Windows"
        architecture = "X64"
        run_id = $RunnerRunId
        run_attempt = $RunnerRunAttempt
        job = $RunnerJob
      }
      stdout = [ordered]@{
        path = "capture.stdout"
        byte_count = [System.IO.FileInfo]::new($stdoutPath).Length
        sha256 = Get-Sha256File $stdoutPath
      }
      stderr = [ordered]@{
        path = "capture.stderr"
        byte_count = [System.IO.FileInfo]::new($stderrPath).Length
        sha256 = Get-Sha256File $stderrPath
      }
    }
    $capturePath = Join-Path $captureDirectory "capture.json"
    Write-Json -Path $capturePath -Value $capture
    $steps.Add([ordered]@{
        ordinal = [int]$plan.ordinal
        step_id = [string]$plan.step_id
        command = "service.$($plan.command)"
        expected_state = [string]$plan.state
        actual_state = [string]$plan.state
        mutation_executed = [bool]$plan.mutation
        capture_path = [System.IO.Path]::GetFullPath($capturePath)
        stdout_path = [System.IO.Path]::GetFullPath($stdoutPath)
      })
  }

  $transcript = [ordered]@{
    format = "eva.windows.platform_service_harness.v1"
    mode = "Lifecycle"
    status = "success"
    source_commit = $SourceCommit
    run_id = $RunId
    service_name = $serviceName
    project_root = [System.IO.Path]::GetFullPath($projectRoot)
    evidence_root = [System.IO.Path]::GetFullPath($evidenceRoot)
    eva_executable = "C:\eva\eva.exe"
    eva_executable_sha256 = $EvaDigest
    authority = [ordered]@{
      allowed = $true
      reasons = [object[]]@()
      is_windows = $true
      is_admin = $true
      execute = $true
      controlled_host = $true
    }
    steps = [object[]]$steps.ToArray()
    continuation = $null
    warnings = [object[]]@()
    written_at = "2026-07-27T00:00:02.0000000+00:00"
  }
  $transcriptPath = Join-Path $evidenceRoot "transcript.lifecycle.json"
  Write-Json -Path $transcriptPath -Value $transcript
  Write-DigestSidecar -JsonPath $transcriptPath

  return [pscustomobject]@{
    Root = $fixtureRoot
    Evidence = $evidenceRoot
    Receipt = $receiptPath
  }
}

function Invoke-Validator {
  param(
    [string]$EvidencePath,
    [string]$ReceiptPath,
    [string]$ExpectedSource = $SourceCommit,
    [string]$ExpectedRun = $RunId,
    [string]$ExpectedIdentity = $RunnerIdentity,
    [string]$ExpectedOsMode = "Lifecycle"
  )

  & $Validator `
    -EvidencePath $EvidencePath `
    -ExpectedSourceCommit $ExpectedSource `
    -ExpectedRunId $ExpectedRun `
    -ExpectedRunnerProvider $RunnerProvider `
    -ExpectedRunnerIdentity $ExpectedIdentity `
    -ExpectedRunnerRunId $RunnerRunId `
    -ExpectedRunnerRunAttempt $RunnerRunAttempt `
    -ExpectedRunnerJob $RunnerJob `
    -ExpectedEvaExecutableSha256 $EvaDigest `
    -Mode $ExpectedOsMode `
    -ReceiptPath $ReceiptPath | Out-Null
}

function Get-CapturePath {
  param(
    [string]$Evidence,
    [int]$Ordinal,
    [string]$StepId
  )

  return Join-Path (Join-Path (Join-Path $Evidence "captures") ("{0:D2}-{1}" -f $Ordinal, $StepId)) "capture.json"
}

function Update-CaptureStreams {
  param([string]$CapturePath)

  $capture = Read-Json $CapturePath
  $directory = [System.IO.Path]::GetDirectoryName($CapturePath)
  $stdoutPath = Join-Path $directory ([string]$capture.stdout.path)
  $stderrPath = Join-Path $directory ([string]$capture.stderr.path)
  $capture.stdout.byte_count = [System.IO.FileInfo]::new($stdoutPath).Length
  $capture.stdout.sha256 = Get-Sha256File $stdoutPath
  $capture.stderr.byte_count = [System.IO.FileInfo]::new($stderrPath).Length
  $capture.stderr.sha256 = Get-Sha256File $stderrPath
  Write-Json -Path $CapturePath -Value $capture
}

function Update-TranscriptDigest {
  param([string]$Evidence)

  Write-DigestSidecar -JsonPath (Join-Path $Evidence "transcript.lifecycle.json")
}

if (-not (Test-Path -LiteralPath $Validator -PathType Leaf)) {
  throw "[platform-service-evidence-test] Validator is missing: $Validator"
}

$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) "eva-platform-service-evidence-$([System.Guid]::NewGuid().ToString('N'))"
[System.IO.Directory]::CreateDirectory($testRoot) | Out-Null

try {
  $valid = New-PlatformServiceFixture -Root $testRoot -Name "valid"
  Invoke-Validator -EvidencePath $valid.Evidence -ReceiptPath $valid.Receipt
  Assert-True (Test-Path -LiteralPath $valid.Receipt -PathType Leaf) "Valid lifecycle must write receipt."
  $receipt = Read-Json $valid.Receipt
  Assert-Equal ([string]$receipt.format) "eva.windows.platform_service_evidence_readback_receipt.v1" "Receipt format changed."
  Assert-Equal ([string]$receipt.status) "verified_local_readback" "Receipt status must stay local readback."
  Assert-Equal ([int]$receipt.capture_count) 13 "Receipt capture count changed."

  $planOnly = New-PlatformServiceFixture -Root $testRoot -Name "plan-only"
  $planTranscriptPath = Join-Path $planOnly.Evidence "transcript.lifecycle.json"
  $planTranscript = Read-Json $planTranscriptPath
  $planTranscript.mode = "Validate"
  $planTranscript.status = "plan_only"
  $planTranscript.warnings = @("non_mutating_validation")
  Write-Json -Path $planTranscriptPath -Value $planTranscript
  Update-TranscriptDigest -Evidence $planOnly.Evidence
  Assert-FailsReason { Invoke-Validator -EvidencePath $planOnly.Evidence -ReceiptPath $planOnly.Receipt } "transcript_mode_invalid" $planOnly.Receipt

  $fake = New-PlatformServiceFixture -Root $testRoot -Name "fake-adapter"
  $fakeCapturePath = Get-CapturePath -Evidence $fake.Evidence -Ordinal 0 -StepId "status-preflight"
  $fakeStdoutPath = Join-Path ([System.IO.Path]::GetDirectoryName($fakeCapturePath)) "capture.stdout"
  $fakeStdout = [System.IO.File]::ReadAllText($fakeStdoutPath, $Utf8NoBom) | ConvertFrom-Json
  $fakeStdout.data.production_adapter = $false
  Write-Utf8LfFile -Path $fakeStdoutPath -Text ($fakeStdout | ConvertTo-Json -Depth 8 -Compress)
  Update-CaptureStreams -CapturePath $fakeCapturePath
  Assert-FailsReason { Invoke-Validator -EvidencePath $fake.Evidence -ReceiptPath $fake.Receipt } "capture_stdout_contract_invalid" $fake.Receipt

  $missingRestart = New-PlatformServiceFixture -Root $testRoot -Name "missing-restart"
  Remove-Item -LiteralPath (Join-Path (Join-Path $missingRestart.Evidence "captures") "06-restart") -Recurse -Force
  Assert-FailsReason { Invoke-Validator -EvidencePath $missingRestart.Evidence -ReceiptPath $missingRestart.Receipt } "capture_missing" $missingRestart.Receipt

  $badFinal = New-PlatformServiceFixture -Root $testRoot -Name "bad-final"
  $badTranscriptPath = Join-Path $badFinal.Evidence "transcript.lifecycle.json"
  $badTranscript = Read-Json $badTranscriptPath
  $badTranscript.steps[12].actual_state = "stopped"
  Write-Json -Path $badTranscriptPath -Value $badTranscript
  Update-TranscriptDigest -Evidence $badFinal.Evidence
  Assert-FailsReason { Invoke-Validator -EvidencePath $badFinal.Evidence -ReceiptPath $badFinal.Receipt } "step_contract_invalid" $badFinal.Receipt

  $wrongSource = New-PlatformServiceFixture -Root $testRoot -Name "wrong-source"
  Assert-FailsReason { Invoke-Validator -EvidencePath $wrongSource.Evidence -ReceiptPath $wrongSource.Receipt -ExpectedSource "ffffffffffffffffffffffffffffffffffffffff" } "owner_invalid" $wrongSource.Receipt

  $wrongRun = New-PlatformServiceFixture -Root $testRoot -Name "wrong-run"
  Assert-FailsReason { Invoke-Validator -EvidencePath $wrongRun.Evidence -ReceiptPath $wrongRun.Receipt -ExpectedRun "ext01-wrong" } "owner_invalid" $wrongRun.Receipt

  $wrongExecutor = New-PlatformServiceFixture -Root $testRoot -Name "wrong-executor"
  Assert-FailsReason { Invoke-Validator -EvidencePath $wrongExecutor.Evidence -ReceiptPath $wrongExecutor.Receipt -ExpectedIdentity "forged/9001/1/platform-service" } "capture_runner_invalid" $wrongExecutor.Receipt

  $wrongOs = New-PlatformServiceFixture -Root $testRoot -Name "wrong-os"
  $wrongOsCapturePath = Get-CapturePath -Evidence $wrongOs.Evidence -Ordinal 0 -StepId "status-preflight"
  $wrongOsCapture = Read-Json $wrongOsCapturePath
  $wrongOsCapture.runner.os = "Linux"
  Write-Json -Path $wrongOsCapturePath -Value $wrongOsCapture
  Assert-FailsReason { Invoke-Validator -EvidencePath $wrongOs.Evidence -ReceiptPath $wrongOs.Receipt } "capture_runner_invalid" $wrongOs.Receipt

  $ownerMismatch = New-PlatformServiceFixture -Root $testRoot -Name "owner-mismatch"
  $ownerPath = Join-Path $ownerMismatch.Evidence "platform-service-harness.owner.json"
  $owner = Read-Json $ownerPath
  $owner.run_id = "ext01-owner"
  Write-Json -Path $ownerPath -Value $owner
  Assert-FailsReason { Invoke-Validator -EvidencePath $ownerMismatch.Evidence -ReceiptPath $ownerMismatch.Receipt } "owner_invalid" $ownerMismatch.Receipt

  $digestTamper = New-PlatformServiceFixture -Root $testRoot -Name "digest-tamper"
  [System.IO.File]::AppendAllText((Join-Path $digestTamper.Evidence "transcript.lifecycle.json"), " ", $Utf8NoBom)
  Assert-FailsReason { Invoke-Validator -EvidencePath $digestTamper.Evidence -ReceiptPath $digestTamper.Receipt } "digest_mismatch" $digestTamper.Receipt

  $captureTamper = New-PlatformServiceFixture -Root $testRoot -Name "capture-tamper"
  $captureTamperPath = Get-CapturePath -Evidence $captureTamper.Evidence -Ordinal 0 -StepId "status-preflight"
  $captureTamperJson = Read-Json $captureTamperPath
  $captureTamperJson.capture_id = "platform-service.forged"
  Write-Json -Path $captureTamperPath -Value $captureTamperJson
  Assert-FailsReason { Invoke-Validator -EvidencePath $captureTamper.Evidence -ReceiptPath $captureTamper.Receipt } "capture_invalid" $captureTamper.Receipt

  $stdoutTamper = New-PlatformServiceFixture -Root $testRoot -Name "stdout-tamper"
  $stdoutTamperCapture = Get-CapturePath -Evidence $stdoutTamper.Evidence -Ordinal 0 -StepId "status-preflight"
  [System.IO.File]::AppendAllText((Join-Path ([System.IO.Path]::GetDirectoryName($stdoutTamperCapture)) "capture.stdout"), "tamper", $Utf8NoBom)
  Assert-FailsReason { Invoke-Validator -EvidencePath $stdoutTamper.Evidence -ReceiptPath $stdoutTamper.Receipt } "capture_stream_size_mismatch" $stdoutTamper.Receipt

  $stderrTamper = New-PlatformServiceFixture -Root $testRoot -Name "stderr-tamper"
  $stderrTamperCapture = Get-CapturePath -Evidence $stderrTamper.Evidence -Ordinal 0 -StepId "status-preflight"
  Write-Utf8LfFile -Path (Join-Path ([System.IO.Path]::GetDirectoryName($stderrTamperCapture)) "capture.stderr") -Text "warning"
  Update-CaptureStreams -CapturePath $stderrTamperCapture
  Assert-FailsReason { Invoke-Validator -EvidencePath $stderrTamper.Evidence -ReceiptPath $stderrTamper.Receipt } "capture_stderr_nonempty" $stderrTamper.Receipt

  $extraFile = New-PlatformServiceFixture -Root $testRoot -Name "extra-file"
  Write-Utf8LfFile -Path (Join-Path $extraFile.Evidence "extra.txt") -Text "extra"
  Assert-FailsReason { Invoke-Validator -EvidencePath $extraFile.Evidence -ReceiptPath $extraFile.Receipt } "file_set_mismatch" $extraFile.Receipt

  $pathEscape = New-PlatformServiceFixture -Root $testRoot -Name "path-escape"
  $escapeTranscriptPath = Join-Path $pathEscape.Evidence "transcript.lifecycle.json"
  $escapeTranscript = Read-Json $escapeTranscriptPath
  $escapeTranscript.steps[0].capture_path = Join-Path $testRoot "outside-capture.json"
  Write-Json -Path $escapeTranscriptPath -Value $escapeTranscript
  Update-TranscriptDigest -Evidence $pathEscape.Evidence
  Assert-FailsReason { Invoke-Validator -EvidencePath $pathEscape.Evidence -ReceiptPath $pathEscape.Receipt } "path_escape" $pathEscape.Receipt

  $receiptInside = New-PlatformServiceFixture -Root $testRoot -Name "receipt-inside"
  $insideReceipt = Join-Path $receiptInside.Evidence "receipt.json"
  Assert-FailsReason { Invoke-Validator -EvidencePath $receiptInside.Evidence -ReceiptPath $insideReceipt } "receipt_inside_evidence" $insideReceipt

  $receiptExists = New-PlatformServiceFixture -Root $testRoot -Name "receipt-exists"
  Write-Utf8LfFile -Path $receiptExists.Receipt -Text "{}"
  Assert-FailsReason { Invoke-Validator -EvidencePath $receiptExists.Evidence -ReceiptPath $receiptExists.Receipt } "receipt_exists" ""

  if ($env:OS -eq "Windows_NT") {
    $reparse = New-PlatformServiceFixture -Root $testRoot -Name "reparse"
    $target = Join-Path $testRoot "reparse-target"
    [System.IO.Directory]::CreateDirectory($target) | Out-Null
    $link = Join-Path $reparse.Evidence "linked"
    try {
      New-Item -ItemType Junction -Path $link -Target $target -ErrorAction Stop | Out-Null
      Assert-FailsReason { Invoke-Validator -EvidencePath $reparse.Evidence -ReceiptPath $reparse.Receipt } "path_reparse_point" $reparse.Receipt
    } catch {
      if ($_.Exception.Message.Contains("[platform-service-evidence-test]") -or $_.Exception.Message.Contains("[platform-service-evidence]")) {
        throw
      }
    } finally {
      if (Test-Path -LiteralPath $link) {
        [System.IO.Directory]::Delete($link)
      }
    }
  }

  $reboot = New-PlatformServiceFixture -Root $testRoot -Name "reboot"
  Assert-FailsReason { Invoke-Validator -EvidencePath $reboot.Evidence -ReceiptPath $reboot.Receipt -ExpectedOsMode "Reboot" } "mode_not_supported" $reboot.Receipt

  Write-Host "Platform service evidence validator self-test passed: valid lifecycle receipt and strict negative readback contract."
} finally {
  if (Test-Path -LiteralPath $testRoot) {
    Remove-Item -LiteralPath $testRoot -Recurse -Force
  }
}
