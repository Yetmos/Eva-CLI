[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$EvidencePath,

  [Parameter(Mandatory = $true)]
  [ValidatePattern("^[0-9a-f]{40}$")]
  [string]$ExpectedSourceCommit,

  [Parameter(Mandatory = $true)]
  [ValidatePattern("^[a-z0-9][a-z0-9-]{3,63}$")]
  [string]$ExpectedRunId,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedRepositoryRoot,

  [Parameter(Mandatory = $true)]
  [ValidateSet("local", "github-actions")]
  [string]$ExpectedRunnerProvider,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedRunnerIdentity,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedRunnerName,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedRunnerArchitecture,

  [string]$ExpectedRunnerRunId = "",
  [string]$ExpectedRunnerRunAttempt = "",
  [string]$ExpectedRunnerJob = "",

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ReceiptPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$BundleSchema = "eva.windows.provider_multiprocess_local_evidence.v1"
$CaptureSchema = "eva.release.command_capture.v1"
$ReceiptSchema = "eva.windows.provider_multiprocess_local_readback_receipt.v1"
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false, $true)

function Fail-Evidence {
  param([string]$Reason, [string]$Detail)

  $safe = if ([string]::IsNullOrWhiteSpace($Detail)) { "none" } else { $Detail.Replace("`r", " ").Replace("`n", " ") }
  throw "[windows-provider-local-readback] reason=$Reason detail=$safe"
}

function Get-FullPath {
  param([string]$Path)

  try {
    if ([string]::IsNullOrWhiteSpace($Path)) {
      Fail-Evidence "path_missing" "path"
    }
    if ([System.IO.Path]::IsPathRooted($Path)) {
      return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $Path))
  } catch {
    if ($_.Exception.Message.StartsWith("[windows-provider-local-readback]", [System.StringComparison]::Ordinal)) {
      throw
    }
    Fail-Evidence "path_invalid" $Path
  }
}

function Get-Comparison {
  if ($env:OS -eq "Windows_NT") {
    return [System.StringComparison]::OrdinalIgnoreCase
  }
  return [System.StringComparison]::Ordinal
}

function Test-PathInside {
  param([string]$Child, [string]$Parent)

  $childFull = Get-FullPath $Child
  $parentFull = (Get-FullPath $Parent).TrimEnd([char[]]@('\', '/'))
  return $childFull.Equals($parentFull, (Get-Comparison)) -or
    $childFull.StartsWith("$parentFull$([System.IO.Path]::DirectorySeparatorChar)", (Get-Comparison))
}

function Assert-NoReparsePath {
  param([string]$Path, [string]$Field)

  $full = Get-FullPath $Path
  $root = [System.IO.Path]::GetPathRoot($full)
  $current = $root
  foreach ($part in $full.Substring($root.Length).Split([char[]]@('\', '/'), [System.StringSplitOptions]::RemoveEmptyEntries)) {
    $current = Join-Path $current $part
    if (-not (Test-Path -LiteralPath $current)) {
      break
    }
    if (([System.IO.File]::GetAttributes($current) -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
      Fail-Evidence "path_reparse_point" ("{0}:{1}" -f $Field, $current)
    }
  }
}

function Assert-NoReparseDescendants {
  param([string]$Path)

  $root = Get-FullPath $Path
  Assert-NoReparsePath -Path $root -Field "evidence_root"
  if (-not (Test-Path -LiteralPath $root -PathType Container)) {
    Fail-Evidence "evidence_root_missing" $root
  }
  foreach ($entry in @(Get-ChildItem -LiteralPath $root -Recurse -Force)) {
    if (($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
      Fail-Evidence "path_reparse_point" $entry.FullName
    }
  }
}

function Read-Utf8 {
  param([string]$Path, [string]$Reason)

  $full = Get-FullPath $Path
  Assert-NoReparsePath -Path $full -Field $Reason
  if (-not (Test-Path -LiteralPath $full -PathType Leaf)) {
    Fail-Evidence $Reason $full
  }
  try {
    return $Utf8NoBom.GetString([System.IO.File]::ReadAllBytes($full))
  } catch {
    Fail-Evidence "utf8_invalid" $full
  }
}

function Read-Json {
  param([string]$Path, [string]$Reason)

  try {
    return (Read-Utf8 -Path $Path -Reason $Reason) | ConvertFrom-Json
  } catch {
    if ($_.Exception.Message.StartsWith("[windows-provider-local-readback]", [System.StringComparison]::Ordinal)) {
      throw
    }
    Fail-Evidence "json_invalid" $Path
  }
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

  $full = Get-FullPath $Path
  Assert-NoReparsePath -Path $full -Field "digest_input"
  if (-not (Test-Path -LiteralPath $full -PathType Leaf)) {
    Fail-Evidence "file_missing" $full
  }
  return Get-Sha256Bytes ([System.IO.File]::ReadAllBytes($full))
}

function Assert-ExactKeys {
  param([object]$Object, [string[]]$Keys, [string]$Reason, [string]$Detail)

  if ($null -eq $Object) {
    Fail-Evidence $Reason $Detail
  }
  $actual = @($Object.PSObject.Properties | ForEach-Object { $_.Name } | Sort-Object)
  $expected = @($Keys | Sort-Object)
  if (($actual -join "`n") -cne ($expected -join "`n")) {
    Fail-Evidence $Reason ("{0}: actual='{1}' expected='{2}'" -f $Detail, ($actual -join ","), ($expected -join ","))
  }
}

function Get-Prop {
  param([object]$Object, [string]$Name, [string]$Reason)

  if ($null -eq $Object -or $null -eq $Object.PSObject.Properties[$Name]) {
    Fail-Evidence $Reason $Name
  }
  return $Object.PSObject.Properties[$Name].Value
}

function Assert-String {
  param([object]$Actual, [string]$Expected, [string]$Reason, [string]$Detail)

  if (-not ($Actual -is [string]) -or [string]$Actual -cne $Expected) {
    Fail-Evidence $Reason ("{0}: actual='{1}' expected='{2}'" -f $Detail, [string]$Actual, $Expected)
  }
}

function Assert-NonEmptyString {
  param([object]$Actual, [string]$Reason, [string]$Detail)

  if (-not ($Actual -is [string]) -or [string]::IsNullOrWhiteSpace([string]$Actual)) {
    Fail-Evidence $Reason $Detail
  }
}

function Assert-Bool {
  param([object]$Actual, [bool]$Expected, [string]$Reason, [string]$Detail)

  if (-not ($Actual -is [bool]) -or [bool]$Actual -ne $Expected) {
    Fail-Evidence $Reason $Detail
  }
}

function Assert-Int {
  param([object]$Actual, [int64]$Expected, [string]$Reason, [string]$Detail)

  if (-not (($Actual -is [byte]) -or ($Actual -is [int16]) -or ($Actual -is [int]) -or ($Actual -is [long])) -or [int64]$Actual -ne $Expected) {
    Fail-Evidence $Reason $Detail
  }
}

function Assert-IntMinimum {
  param([object]$Actual, [int64]$Minimum, [string]$Reason, [string]$Detail)

  if (-not (($Actual -is [byte]) -or ($Actual -is [int16]) -or ($Actual -is [int]) -or ($Actual -is [long])) -or [int64]$Actual -lt $Minimum) {
    Fail-Evidence $Reason $Detail
  }
}

function Assert-Null {
  param([object]$Actual, [string]$Reason, [string]$Detail)

  if ($null -ne $Actual) {
    Fail-Evidence $Reason $Detail
  }
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
  $credentialScript = Join-Path $repositoryFull "scripts\test-credential-leak-scan.ps1"
  return @(
    (New-Plan 0 "process-boundary" "cargo" @("test", "-p", "eva-adapter", "process_backend::tests::", "--", "--test-threads=1", "--nocapture") @("pid_start_token", "windows_job", "descendant_cleanup", "pid_reuse_fence", "same_sid", "different_sid_reject", "unknown_sid_reject") @("windows_same_token_identity_spawns_inside_job_boundary", "windows_distinct_service_token_is_rejected_before_spawn", "windows_unknown_account_is_rejected_before_spawn", "12 passed; 0 failed; 3 ignored")),
    (New-Plan 1 "restart-budget" "cargo" @("test", "-p", "eva-adapter", "runtime::tests::runtime_crash_loop_never_exceeds_durable_restart_budget", "--", "--exact", "--test-threads=1", "--nocapture") @("bounded_restart_attempts", "budget_exhausted", "durable_restart_state") @("runtime_crash_loop_never_exceeds_durable_restart_budget", "1 passed; 0 failed")),
    (New-Plan 2 "provider-admission" "cargo" @("test", "-p", "eva-storage", "provider_admission::tests::", "--", "--test-threads=1", "--nocapture") @("capacity_one_winner", "crash_expiry_reclaim", "successor_fence") @("two_processes_have_one_winner_for_capacity_one", "crashed_process_reservation_is_reclaimed_only_after_expiry", "6 passed; 0 failed")),
    (New-Plan 3 "orphan-recovery" "cargo" @("test", "-p", "eva-runtime", "recovery::tests::", "--", "--test-threads=1", "--nocapture") @("orphan_cleanup", "pid_reuse_reject", "legacy_identity_reject", "generation_restart_preserved") @("recovery_interrupts_active_provider_process_and_preserves_task", "recovery_refuses_pid_reuse_without_mutating_task_or_process_record", "recovery_preserves_auto_restart_attempt_after_orphan_cleanup_and_generation_change", "23 passed; 0 failed")),
    (New-Plan 4 "daemon-recovery" "cargo" @("test", "-p", "eva-runtime", "daemon::tests::daemon_start_recovers_interrupted_provider_process_state", "--", "--exact", "--test-threads=1", "--nocapture") @("daemon_restart_cleanup", "interrupted_provider_retired") @("daemon_start_recovers_interrupted_provider_process_state", "1 passed; 0 failed")),
    (New-Plan 5 "credential-secret-zero" "powershell" @("-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", $credentialScript, "-RepositoryRoot", $repositoryFull) @("synthetic_canary_consumed", "stdout_stderr_redacted", "artifact_redacted", "scanner_negative_controls") @("Credential leak scan harness passed: 8 credential cases and 3 scanner negative controls."))
  )
}

function Join-SafeRelative {
  param([string]$Root, [string]$Relative, [string]$Field)

  if ([string]::IsNullOrWhiteSpace($Relative) -or [System.IO.Path]::IsPathRooted($Relative) -or $Relative -match '(^|[\/])\.\.([\/]|$)') {
    Fail-Evidence "path_escape" ("{0}:{1}" -f $Field, $Relative)
  }
  $full = Get-FullPath (Join-Path $Root $Relative)
  if (-not (Test-PathInside -Child $full -Parent $Root)) {
    Fail-Evidence "path_escape" ("{0}:{1}" -f $Field, $Relative)
  }
  Assert-NoReparsePath -Path $full -Field $Field
  return $full
}

function Assert-Runner {
  param([object]$Runner, [string]$Reason)

  Assert-ExactKeys $Runner @("provider", "identity", "name", "os", "architecture", "run_id", "run_attempt", "job") $Reason "runner"
  Assert-String (Get-Prop $Runner "provider" $Reason) $ExpectedRunnerProvider $Reason "provider"
  Assert-String (Get-Prop $Runner "identity" $Reason) $ExpectedRunnerIdentity $Reason "identity"
  Assert-String (Get-Prop $Runner "name" $Reason) $ExpectedRunnerName $Reason "name"
  Assert-String (Get-Prop $Runner "os" $Reason) "Windows" $Reason "os"
  Assert-String (Get-Prop $Runner "architecture" $Reason) $ExpectedRunnerArchitecture $Reason "architecture"
  foreach ($binding in @(
      @{ Name = "run_id"; Expected = $ExpectedRunnerRunId },
      @{ Name = "run_attempt"; Expected = $ExpectedRunnerRunAttempt },
      @{ Name = "job"; Expected = $ExpectedRunnerJob }
    )) {
    $actual = Get-Prop $Runner ([string]$binding.Name) $Reason
    if ([string]::IsNullOrEmpty([string]$binding.Expected)) {
      Assert-Null $actual $Reason ([string]$binding.Name)
    } else {
      Assert-String $actual ([string]$binding.Expected) $Reason ([string]$binding.Name)
    }
  }
}

function Assert-Stream {
  param([object]$Stream, [string]$Directory, [string]$Name, [string]$Reason)

  Assert-ExactKeys $Stream @("path", "byte_count", "sha256") $Reason $Name
  Assert-String (Get-Prop $Stream "path" $Reason) $Name $Reason "$Name.path"
  $path = Join-SafeRelative -Root $Directory -Relative $Name -Field $Name
  if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
    Fail-Evidence "capture_stream_missing" $Name
  }
  $bytes = [System.IO.File]::ReadAllBytes($path)
  Assert-Int (Get-Prop $Stream "byte_count" $Reason) ([int64]$bytes.Length) $Reason "$Name.byte_count"
  Assert-String (Get-Prop $Stream "sha256" $Reason) (Get-Sha256Bytes $bytes) $Reason "$Name.sha256"
  try {
    return $Utf8NoBom.GetString($bytes)
  } catch {
    Fail-Evidence "capture_stream_utf8_invalid" $Name
  }
}

function Assert-StringArray {
  param([object]$Actual, [string[]]$Expected, [string]$Reason, [string]$Detail)

  $values = @($Actual)
  if ($values.Count -ne $Expected.Count) {
    Fail-Evidence $Reason "$Detail.count"
  }
  for ($index = 0; $index -lt $Expected.Count; $index += 1) {
    Assert-String $values[$index] $Expected[$index] $Reason "$Detail[$index]"
  }
}

function Write-NewJson {
  param([string]$Path, [object]$Value)

  $full = Get-FullPath $Path
  Assert-NoReparsePath -Path $full -Field "receipt"
  $parent = [System.IO.Path]::GetDirectoryName($full)
  if ([string]::IsNullOrWhiteSpace($parent)) {
    Fail-Evidence "receipt_path_invalid" $full
  }
  Assert-NoReparsePath -Path $parent -Field "receipt_parent"
  [System.IO.Directory]::CreateDirectory($parent) | Out-Null
  try {
    $stream = New-Object System.IO.FileStream($full, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    try {
      $json = (($Value | ConvertTo-Json -Depth 16 -Compress).Replace("`r`n", "`n").Replace("`r", "`n"))
      $bytes = $Utf8NoBom.GetBytes("$json`n")
      $stream.Write($bytes, 0, $bytes.Length)
      $stream.Flush($true)
    } finally {
      $stream.Dispose()
    }
  } catch {
    Fail-Evidence "receipt_exists" $full
  }
}

if ($ExpectedRunnerProvider -eq "github-actions") {
  foreach ($value in @($ExpectedRunnerRunId, $ExpectedRunnerRunAttempt, $ExpectedRunnerJob)) {
    if ([string]::IsNullOrWhiteSpace($value)) {
      Fail-Evidence "runner_binding_missing" "github-actions"
    }
  }
} elseif (-not [string]::IsNullOrEmpty($ExpectedRunnerRunId) -or -not [string]::IsNullOrEmpty($ExpectedRunnerRunAttempt) -or -not [string]::IsNullOrEmpty($ExpectedRunnerJob)) {
  Fail-Evidence "runner_binding_invalid" "local"
}

$root = Get-FullPath $EvidencePath
$receiptFull = Get-FullPath $ReceiptPath
$repositoryFull = Get-FullPath $ExpectedRepositoryRoot
Assert-NoReparseDescendants -Path $root
Assert-NoReparsePath -Path $repositoryFull -Field "repository_root"
if (-not (Test-Path -LiteralPath $repositoryFull -PathType Container)) {
  Fail-Evidence "repository_missing" $repositoryFull
}
if (Test-PathInside -Child $receiptFull -Parent $root) {
  Fail-Evidence "receipt_inside_evidence" $receiptFull
}
Assert-NoReparsePath -Path $receiptFull -Field "receipt"
if (Test-Path -LiteralPath $receiptFull) {
  Fail-Evidence "receipt_exists" $receiptFull
}

$bundlePath = Join-Path $root "bundle.json"
$bundleDigestPath = Join-Path $root "bundle.sha256"
$bundleDigest = Get-Sha256File $bundlePath
$sidecar = Read-Utf8 -Path $bundleDigestPath -Reason "bundle_digest_missing"
if ($sidecar.Length -ne 72 -or $sidecar.Substring(71, 1) -cne "`n" -or $sidecar.Substring(0, 71) -cnotmatch '^sha256:[0-9a-f]{64}$') {
  Fail-Evidence "bundle_digest_invalid" $bundleDigestPath
}
Assert-String $sidecar.Substring(0, 71) $bundleDigest "bundle_digest_mismatch" "bundle"
$bundle = Read-Json -Path $bundlePath -Reason "bundle_missing"
Assert-ExactKeys $bundle @("schema", "status", "source_commit", "run_id", "repository_root", "scope", "runner", "scenarios", "written_at") "bundle_invalid" "bundle"
Assert-String (Get-Prop $bundle "schema" "bundle_invalid") $BundleSchema "bundle_invalid" "schema"
Assert-String (Get-Prop $bundle "status" "bundle_invalid") "completed_repository_local" "bundle_invalid" "status"
Assert-String (Get-Prop $bundle "source_commit" "bundle_invalid") $ExpectedSourceCommit "bundle_invalid" "source_commit"
Assert-String (Get-Prop $bundle "run_id" "bundle_invalid") $ExpectedRunId "bundle_invalid" "run_id"
Assert-String (Get-Prop $bundle "repository_root" "bundle_invalid") $repositoryFull "bundle_invalid" "repository_root"
Assert-NonEmptyString (Get-Prop $bundle "written_at" "bundle_invalid") "bundle_invalid" "written_at"
$scope = Get-Prop $bundle "scope" "scope_invalid"
Assert-ExactKeys $scope @("os", "production", "service_identity_attested", "real_vault_authority", "credential_material") "scope_invalid" "scope"
Assert-String (Get-Prop $scope "os" "scope_invalid") "Windows" "scope_invalid" "os"
Assert-Bool (Get-Prop $scope "production" "scope_invalid") $false "scope_invalid" "production"
Assert-Bool (Get-Prop $scope "service_identity_attested" "scope_invalid") $false "scope_invalid" "service_identity_attested"
Assert-Bool (Get-Prop $scope "real_vault_authority" "scope_invalid") $false "scope_invalid" "real_vault_authority"
Assert-String (Get-Prop $scope "credential_material" "scope_invalid") "synthetic_canary" "scope_invalid" "credential_material"
Assert-Runner -Runner (Get-Prop $bundle "runner" "runner_invalid") -Reason "runner_invalid"

$plans = @(Get-Plans)
$scenarios = @(Get-Prop $bundle "scenarios" "scenario_invalid")
if ($scenarios.Count -ne $plans.Count) {
  Fail-Evidence "scenario_count_invalid" ([string]$scenarios.Count)
}
$expectedFiles = New-Object System.Collections.Generic.List[string]
$expectedDirectories = New-Object System.Collections.Generic.List[string]
$expectedFiles.Add("bundle.json")
$expectedFiles.Add("bundle.sha256")
$expectedDirectories.Add("captures")
foreach ($plan in $plans) {
  $scenario = $scenarios[[int]$plan.ordinal]
  Assert-ExactKeys $scenario @("ordinal", "id", "capture_path", "claims") "scenario_invalid" ([string]$plan.id)
  Assert-Int (Get-Prop $scenario "ordinal" "scenario_invalid") ([int64]$plan.ordinal) "scenario_invalid" "$($plan.id).ordinal"
  Assert-String (Get-Prop $scenario "id" "scenario_invalid") ([string]$plan.id) "scenario_invalid" "$($plan.id).id"
  $directory = "captures/{0:D2}-{1}" -f [int]$plan.ordinal, [string]$plan.id
  $captureRelative = "$directory/capture.json"
  Assert-String (Get-Prop $scenario "capture_path" "scenario_invalid") $captureRelative "scenario_invalid" "$($plan.id).capture_path"
  Assert-StringArray (Get-Prop $scenario "claims" "scenario_invalid") ([string[]]$plan.claims) "scenario_invalid" "$($plan.id).claims"
  $expectedDirectories.Add($directory)
  foreach ($name in @("capture.json", "capture.stdout", "capture.stderr")) {
    $expectedFiles.Add("$directory/$name")
  }

  $capturePath = Join-SafeRelative -Root $root -Relative $captureRelative -Field "$($plan.id).capture"
  $captureDirectory = [System.IO.Path]::GetDirectoryName($capturePath)
  $capture = Read-Json -Path $capturePath -Reason "capture_missing"
  Assert-ExactKeys $capture @("format", "capture_id", "executable", "argv", "outcome", "started_at", "finished_at", "duration_ms", "exit_code", "failure_reason", "runner", "stdout", "stderr") "capture_invalid" ([string]$plan.id)
  Assert-String (Get-Prop $capture "format" "capture_invalid") $CaptureSchema "capture_invalid" "$($plan.id).format"
  Assert-String (Get-Prop $capture "capture_id" "capture_invalid") "w3-windows-local.$($plan.id)" "capture_invalid" "$($plan.id).capture_id"
  Assert-String (Get-Prop $capture "executable" "capture_invalid") ([string]$plan.executable) "capture_invalid" "$($plan.id).executable"
  Assert-StringArray (Get-Prop $capture "argv" "capture_invalid") ([string[]]$plan.argv) "capture_invalid" "$($plan.id).argv"
  Assert-String (Get-Prop $capture "outcome" "capture_invalid") "success" "capture_invalid" "$($plan.id).outcome"
  Assert-NonEmptyString (Get-Prop $capture "started_at" "capture_invalid") "capture_invalid" "$($plan.id).started_at"
  Assert-NonEmptyString (Get-Prop $capture "finished_at" "capture_invalid") "capture_invalid" "$($plan.id).finished_at"
  Assert-IntMinimum (Get-Prop $capture "duration_ms" "capture_invalid") 0 "capture_invalid" "$($plan.id).duration_ms"
  Assert-Int (Get-Prop $capture "exit_code" "capture_invalid") 0 "capture_invalid" "$($plan.id).exit_code"
  Assert-Null (Get-Prop $capture "failure_reason" "capture_invalid") "capture_invalid" "$($plan.id).failure_reason"
  Assert-Runner -Runner (Get-Prop $capture "runner" "capture_runner_invalid") -Reason "capture_runner_invalid"
  $stdout = Assert-Stream -Stream (Get-Prop $capture "stdout" "capture_stream_invalid") -Directory $captureDirectory -Name "capture.stdout" -Reason "capture_stream_invalid"
  $stderr = Assert-Stream -Stream (Get-Prop $capture "stderr" "capture_stream_invalid") -Directory $captureDirectory -Name "capture.stderr" -Reason "capture_stream_invalid"
  $combined = "$stdout`n$stderr"
  foreach ($marker in [string[]]$plan.markers) {
    if (-not $combined.Contains($marker)) {
      Fail-Evidence "scenario_marker_missing" ("{0}:{1}" -f $plan.id, $marker)
    }
  }
}

$actualFiles = @(Get-ChildItem -LiteralPath $root -Recurse -Force -File | ForEach-Object { $_.FullName.Substring($root.Length).TrimStart([char[]]@('\', '/')).Replace('\', '/') } | Sort-Object)
$actualDirectories = @(Get-ChildItem -LiteralPath $root -Recurse -Force -Directory | ForEach-Object { $_.FullName.Substring($root.Length).TrimStart([char[]]@('\', '/')).Replace('\', '/') } | Sort-Object)
if (($actualFiles -join "`n") -cne (@($expectedFiles | Sort-Object) -join "`n")) {
  Fail-Evidence "file_set_invalid" "files"
}
if (($actualDirectories -join "`n") -cne (@($expectedDirectories | Sort-Object) -join "`n")) {
  Fail-Evidence "file_set_invalid" "directories"
}
$treeLines = @($actualFiles | ForEach-Object { "$_ $(Get-Sha256File (Join-Path $root $_))" })
$treeDigest = Get-Sha256Bytes ($Utf8NoBom.GetBytes(($treeLines -join "`n") + "`n"))

$receipt = [ordered]@{
  schema = $ReceiptSchema
  status = "verified_repository_local_readback"
  source_commit = $ExpectedSourceCommit
  run_id = $ExpectedRunId
  repository_root = $repositoryFull
  runner = Get-Prop $bundle "runner" "runner_invalid"
  scenario_count = $plans.Count
  bundle_digest = $bundleDigest
  tree_digest = $treeDigest
  production = $false
  verified_at = [System.DateTimeOffset]::UtcNow.ToString("o", [System.Globalization.CultureInfo]::InvariantCulture)
}
Write-NewJson -Path $receiptFull -Value $receipt
Write-Output $receiptFull
