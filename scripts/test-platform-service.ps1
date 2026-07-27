[CmdletBinding()]
param(
  [ValidateSet("Validate", "Lifecycle", "PrepareReboot", "ResumeReboot")]
  [string]$Mode = "Validate",
  [string]$RunId = "ext01-smoke",
  [string]$SourceCommit = "",
  [string]$RepositoryRoot = "",
  [string]$ProjectRoot,
  [string]$EvidenceRoot,
  [string]$EvaExecutable,
  [switch]$Execute,
  [switch]$ControlledHost,
  [switch]$AllowExternalContinuation,
  [string]$ExpectedBootMarker,
  [string]$ContinuationPath,
  [string]$ExpectedContinuationDigest,
  [string]$CurrentBootMarker,
  [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "test-platform-service-harness.ps1")

if ([string]::IsNullOrWhiteSpace($RepositoryRoot)) {
  $RepositoryRoot = Join-Path $PSScriptRoot ".."
}

function Assert-True {
  param(
    [bool]$Condition,
    [string]$Message
  )

  if (-not $Condition) {
    throw "[platform-service-harness-test] $Message"
  }
}

function Assert-Equal {
  param(
    [object]$Actual,
    [object]$Expected,
    [string]$Message
  )

  if ([string]$Actual -cne [string]$Expected) {
    throw "[platform-service-harness-test] $Message actual='$Actual' expected='$Expected'"
  }
}

function Assert-ThrowsReason {
  param(
    [scriptblock]$Action,
    [string]$Reason
  )

  try {
    & $Action
  } catch {
    $message = $_.Exception.ToString()
    Assert-True $message.Contains("reason=$Reason") "Expected reason '$Reason', got: $message"
    return
  }
  throw "[platform-service-harness-test] Expected reason '$Reason', but the action succeeded."
}

function New-TestPaths {
  param(
    [string]$Root,
    [string]$RunId
  )

  return [pscustomobject]@{
    project = Join-Path $Root "project-$RunId"
    evidence = Join-Path $Root "evidence-$RunId"
  }
}

function Invoke-SelfTest {
  $repositoryRoot = (Get-FullPath $RepositoryRoot)
  $testCommit = "0123456789abcdef0123456789abcdef01234567"
  $testRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("eva-platform-service-tests-" + [System.Guid]::NewGuid().ToString("N"))
  [System.IO.Directory]::CreateDirectory($testRoot) | Out-Null

  try {
    $goodRunId = "ext01-test"
    $paths = New-TestPaths -Root $testRoot -RunId $goodRunId

    Assert-ThrowsReason {
      New-PlatformServiceContext -RepositoryRoot $repositoryRoot -RunId $goodRunId -ProjectRoot (Join-Path $testRoot "project-no-scope") -EvidenceRoot $paths.evidence -EvaExecutable ""
    } "run_scope_missing"

    Assert-ThrowsReason {
      New-PlatformServiceContext -RepositoryRoot $repositoryRoot -RunId $goodRunId -ProjectRoot $paths.project -EvidenceRoot (Join-Path $testRoot "evidence-no-scope") -EvaExecutable ""
    } "run_scope_missing"

    $lifecyclePlan = Get-LifecyclePlan -Mode "Lifecycle"
    Assert-Equal ((@($lifecyclePlan | ForEach-Object { $_.command }) -join ",")) "status,install,install,start,start,status,restart,status,stop,stop,uninstall,uninstall,status" "Lifecycle command order changed."
    Assert-Equal ([string]$lifecyclePlan[0].expected_state) "not_installed" "Preflight state changed."
    Assert-Equal ([string]$lifecyclePlan[12].expected_state) "not_installed" "Final state changed."

    $foreignRunId = "ext01-foreign"
    $foreignPaths = New-TestPaths -Root $testRoot -RunId $foreignRunId
    [System.IO.Directory]::CreateDirectory($foreignPaths.project) | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $foreignPaths.project "foreign.txt"), "do not touch")
    Assert-ThrowsReason {
      New-HarnessProjectLayout -RepositoryRoot $repositoryRoot -RunId $foreignRunId -ProjectRoot $foreignPaths.project
    } "project_not_harness_owned"
    Assert-True (Test-Path -LiteralPath (Join-Path $foreignPaths.project "foreign.txt") -PathType Leaf) "Foreign project content must remain untouched."

    $reparseTarget = Join-Path $testRoot "reparse-target"
    $reparsePath = Join-Path $testRoot "ext01-reparse-project"
    [System.IO.Directory]::CreateDirectory($reparseTarget) | Out-Null
    try {
      New-Item -ItemType Junction -Path $reparsePath -Target $reparseTarget -ErrorAction Stop | Out-Null
      Assert-ThrowsReason {
        Assert-NoReparsePath -Path $reparsePath -Field "project_root"
      } "path_reparse_point"
    } catch {
      if ($_.Exception.Message.Contains("[platform-service-harness-test]") -or $_.Exception.Message.Contains("[platform-service-harness]")) {
        throw
      }
    }

    $authority = Get-HarnessAuthority -Execute:$false -ControlledHost:$false -IsWindows:$true -IsAdmin:$false
    Assert-True (-not $authority.allowed) "Missing authority must refuse mutation."
    Assert-Equal (($authority.reasons -join ",")) "execute_required,controlled_host_required,elevated_admin_required" "Authority refusal reasons changed."
    Assert-ThrowsReason {
      Assert-HarnessAuthority -Execute:$false -ControlledHost:$false
    } "mutation_not_authorized"

    $refusalRunId = "ext01-refusal"
    $refusalPaths = New-TestPaths -Root $testRoot -RunId $refusalRunId
    Assert-ThrowsReason {
      Invoke-PlatformServiceMode `
        -Mode "Lifecycle" `
        -RepositoryRoot $repositoryRoot `
        -RunId $refusalRunId `
        -SourceCommit $testCommit `
        -ProjectRoot $refusalPaths.project `
        -EvidenceRoot $refusalPaths.evidence `
        -Execute:$false `
        -ControlledHost:$false
    } "mutation_not_authorized"
    Assert-True (-not (Test-Path -LiteralPath $refusalPaths.project)) "Authority refusal must occur before fixture creation."
    Assert-True (-not (Test-Path -LiteralPath $refusalPaths.evidence)) "Authority refusal must occur before evidence creation."

    $foreignEvidenceRunId = "ext01-foreign-evidence"
    $foreignEvidencePaths = New-TestPaths -Root $testRoot -RunId $foreignEvidenceRunId
    $foreignEvidenceContext = New-PlatformServiceContext -RepositoryRoot $repositoryRoot -RunId $foreignEvidenceRunId -SourceCommit $testCommit -ProjectRoot $foreignEvidencePaths.project -EvidenceRoot $foreignEvidencePaths.evidence -EvaExecutable ""
    [System.IO.Directory]::CreateDirectory($foreignEvidenceContext.evidence_root) | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $foreignEvidenceContext.evidence_root "foreign.txt"), "do not touch")
    Assert-ThrowsReason {
      Initialize-HarnessEvidenceLayout -Context $foreignEvidenceContext
    } "evidence_not_harness_owned"
    Assert-True (Test-Path -LiteralPath (Join-Path $foreignEvidenceContext.evidence_root "foreign.txt") -PathType Leaf) "Foreign evidence content must remain untouched."

    $configJunctionRunId = "ext01-config-junction"
    $configJunctionPaths = New-TestPaths -Root $testRoot -RunId $configJunctionRunId
    New-HarnessProjectLayout -RepositoryRoot $repositoryRoot -RunId $configJunctionRunId -ProjectRoot $configJunctionPaths.project
    $configJunctionPath = Join-Path $configJunctionPaths.project "config"
    $configJunctionTarget = Join-Path $testRoot "config-junction-target"
    [System.IO.Directory]::CreateDirectory($configJunctionTarget) | Out-Null
    Remove-Item -LiteralPath $configJunctionPath -Recurse -Force
    New-Item -ItemType Junction -Path $configJunctionPath -Target $configJunctionTarget -ErrorAction Stop | Out-Null
    try {
      Assert-ThrowsReason {
        New-HarnessProjectLayout -RepositoryRoot $repositoryRoot -RunId $configJunctionRunId -ProjectRoot $configJunctionPaths.project
      } "path_reparse_point"
      Assert-True (-not (Test-Path -LiteralPath (Join-Path $configJunctionTarget "eva.yaml") -PathType Leaf)) "Project config junction must not receive harness writes."
    } finally {
      if (Test-Path -LiteralPath $configJunctionPath) {
        [System.IO.Directory]::Delete($configJunctionPath)
      }
    }

    $captureJunctionRunId = "ext01-capture-junction"
    $captureJunctionPaths = New-TestPaths -Root $testRoot -RunId $captureJunctionRunId
    $captureJunctionContext = New-PlatformServiceContext -RepositoryRoot $repositoryRoot -RunId $captureJunctionRunId -SourceCommit $testCommit -ProjectRoot $captureJunctionPaths.project -EvidenceRoot $captureJunctionPaths.evidence -EvaExecutable ""
    $captureJunctionTarget = Join-Path $testRoot "capture-junction-target"
    [System.IO.Directory]::CreateDirectory($captureJunctionContext.evidence_root) | Out-Null
    [System.IO.Directory]::CreateDirectory($captureJunctionTarget) | Out-Null
    New-Item -ItemType Junction -Path $captureJunctionContext.capture_root -Target $captureJunctionTarget -ErrorAction Stop | Out-Null
    try {
      Assert-ThrowsReason {
        Initialize-HarnessEvidenceLayout -Context $captureJunctionContext
      } "path_reparse_point"
      Assert-Equal (@(Get-ChildItem -LiteralPath $captureJunctionTarget -Force).Count) 0 "Capture junction target must remain untouched."
    } finally {
      if (Test-Path -LiteralPath $captureJunctionContext.capture_root) {
        [System.IO.Directory]::Delete($captureJunctionContext.capture_root)
      }
    }

    $captureCollisionRunId = "ext01-capture-collision"
    $captureCollisionPaths = New-TestPaths -Root $testRoot -RunId $captureCollisionRunId
    $captureCollisionContext = New-PlatformServiceContext -RepositoryRoot $repositoryRoot -RunId $captureCollisionRunId -SourceCommit $testCommit -ProjectRoot $captureCollisionPaths.project -EvidenceRoot $captureCollisionPaths.evidence -EvaExecutable ""
    Initialize-HarnessEvidenceLayout -Context $captureCollisionContext
    $captureCollisionStep = Get-StepDefinition 0 "status-preflight" "status" "not_installed" $false
    [System.IO.Directory]::CreateDirectory((Join-Path $captureCollisionContext.capture_root "00-status-preflight")) | Out-Null
    Assert-ThrowsReason {
      Invoke-CapturedServiceCommand -Context $captureCollisionContext -Step $captureCollisionStep
    } "immutable_capture_exists"

    $continuationPaths = New-TestPaths -Root $testRoot -RunId "ext01-cont"
    $context = New-PlatformServiceContext -RepositoryRoot $repositoryRoot -RunId "ext01-cont" -SourceCommit $testCommit -ProjectRoot $continuationPaths.project -EvidenceRoot $continuationPaths.evidence -EvaExecutable ""
    New-HarnessProjectLayout -RepositoryRoot $repositoryRoot -RunId "ext01-cont" -ProjectRoot $context.project_root -StartOnBoot
    $projectConfig = Join-Path (Join-Path $context.project_root "config") "eva.yaml"
    Assert-True (Test-Path -LiteralPath $projectConfig -PathType Leaf) "Harness config must be written below config/eva.yaml."
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $context.project_root "eva.yaml") -PathType Leaf)) "Harness must not write an unused root eva.yaml."
    Assert-True ([System.IO.File]::ReadAllText($projectConfig).Contains("start_on_boot: true")) "Reboot fixture must enable boot start."
    [System.IO.Directory]::CreateDirectory($context.evidence_root) | Out-Null
    $manifest = [ordered]@{
      format = "eva.windows.platform_service_continuation.v1"
      source_commit = $testCommit
      run_id = "ext01-cont"
      service_name = "eva-ext01-ext01-cont"
      project_root = $context.project_root
      evidence_root = $context.evidence_root
      eva_executable = $context.eva_invocation.executable
      eva_executable_sha256 = $context.eva_executable_sha256
      prepared_boot_marker = "boot-a"
      expected_boot_marker = "boot-b"
      prepared_at = [System.DateTimeOffset]::UtcNow.ToString("o", [System.Globalization.CultureInfo]::InvariantCulture)
    }
    Write-Utf8LfJson -Path $context.continuation_path -Value $manifest
    $digest = Get-Sha256File $context.continuation_path
    Assert-ThrowsReason {
      Write-NewUtf8LfJson -Path $context.continuation_path -Value $manifest
    } "immutable_artifact_exists"

    $tampered = [ordered]@{}
    foreach ($property in $manifest.Keys) {
      $tampered[$property] = $manifest[$property]
    }
    $tampered.expected_boot_marker = "boot-c"
    Write-Utf8LfJson -Path $context.continuation_path -Value $tampered
    Assert-ThrowsReason {
      Read-ContinuationArtifacts -ContinuationPath $context.continuation_path -ExpectedDigest $digest
    } "continuation_digest_mismatch"

    Write-Utf8LfJson -Path $context.continuation_path -Value $manifest
    $digest = Get-Sha256File $context.continuation_path
    $readBack = Read-ContinuationArtifacts -ContinuationPath $context.continuation_path -ExpectedDigest $digest
    Assert-ThrowsReason {
      Assert-ResumeBinding -Context $context -Continuation $readBack -ExpectedDigest $digest -CurrentBootMarker "boot-a"
    } "continuation_boot_marker_unchanged"
    Assert-ThrowsReason {
      Assert-ResumeBinding -Context $context -Continuation $readBack -ExpectedDigest $digest -CurrentBootMarker "boot-z"
    } "continuation_boot_marker_mismatch"
    $resume = Assert-ResumeBinding -Context $context -Continuation $readBack -ExpectedDigest $digest -CurrentBootMarker "boot-b"
    Assert-Equal ([string]$resume.current_boot_marker) "boot-b" "Resume boot marker changed."

    $cleanupErrors = Invoke-HarnessCleanup -Context $context -CleanupActions @(
      { throw "cleanup exploded" }
    )
    Assert-Equal (@($cleanupErrors).Count) 1 "Cleanup error count changed."
    Assert-ThrowsReason {
      Complete-HarnessResult -OperationSucceeded:$true -CleanupErrors $cleanupErrors -OperationName "Lifecycle"
    } "cleanup_failed"

    $validateRunId = "ext01-validate"
    $validatePaths = New-TestPaths -Root $testRoot -RunId $validateRunId
    $validateResult = Invoke-PlatformServiceMode `
      -Mode "Validate" `
      -RepositoryRoot $repositoryRoot `
      -RunId $validateRunId `
      -SourceCommit $testCommit `
      -ProjectRoot $validatePaths.project `
      -EvidenceRoot $validatePaths.evidence `
      -Execute:$false `
      -ControlledHost:$false
    Assert-Equal ([string]$validateResult.transcript.status) "plan_only" "Validate mode must not invoke SCM."
    Assert-Equal (@($validateResult.transcript.steps).Count) 13 "Validate mode must emit full lifecycle plan."
    Assert-True ((Test-Path -LiteralPath $validateResult.path -PathType Leaf)) "Validate transcript missing."
    Assert-True ((Test-Path -LiteralPath $validateResult.digest_path -PathType Leaf)) "Validate transcript digest missing."
    $validateContext = New-PlatformServiceContext -RepositoryRoot $repositoryRoot -RunId $validateRunId -SourceCommit $testCommit -ProjectRoot $validatePaths.project -EvidenceRoot $validatePaths.evidence -EvaExecutable ""
    Assert-ThrowsReason {
      Write-TranscriptArtifacts -Context $validateContext -Mode "Validate" -Status "plan_only" -Steps @() -Authority $authority -Continuation $null -Warnings @()
    } "immutable_artifact_exists"

    Write-Host "Platform service harness self-test passed: run scoping, evidence ownership and immutability, reparse refusal, canonical order, authority gate, continuation validation, cleanup override, and non-SCM validation."
  } finally {
    if (Test-Path -LiteralPath $testRoot) {
      Remove-Item -LiteralPath $testRoot -Recurse -Force
    }
  }
}

if ($SelfTest) {
  Invoke-SelfTest
  exit 0
}

if ([string]::IsNullOrWhiteSpace($ProjectRoot)) {
  $ProjectRoot = Join-Path ([System.IO.Path]::GetTempPath()) "eva-ext01-$RunId-project-$RunId"
}
if ([string]::IsNullOrWhiteSpace($EvidenceRoot)) {
  $EvidenceRoot = Join-Path ([System.IO.Path]::GetTempPath()) "eva-ext01-$RunId-evidence-$RunId"
}

$result = Invoke-PlatformServiceMode `
  -Mode $Mode `
  -RepositoryRoot $RepositoryRoot `
  -RunId $RunId `
  -SourceCommit $SourceCommit `
  -ProjectRoot $ProjectRoot `
  -EvidenceRoot $EvidenceRoot `
  -EvaExecutable $EvaExecutable `
  -Execute:$Execute `
  -ControlledHost:$ControlledHost `
  -AllowExternalContinuation:$AllowExternalContinuation `
  -ExpectedBootMarker $ExpectedBootMarker `
  -ContinuationPath $ContinuationPath `
  -ExpectedContinuationDigest $ExpectedContinuationDigest `
  -CurrentBootMarker $CurrentBootMarker

$result.transcript | ConvertTo-Json -Depth 12
