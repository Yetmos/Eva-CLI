[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$EvidencePath,

  [Parameter(Mandatory = $true)]
  [ValidatePattern("^[0-9a-f]{40}$")]
  [string]$ExpectedSourceCommit,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedRepositoryRoot,

  [Parameter(Mandatory = $true)]
  [ValidatePattern("^[a-z0-9][a-z0-9-]{3,63}$")]
  [string]$ExpectedRunId,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedRunnerProvider,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedRunnerIdentity,

  [Parameter(Mandatory = $true)]
  [ValidatePattern("^[1-9][0-9]*$")]
  [string]$ExpectedRunnerRunId,

  [Parameter(Mandatory = $true)]
  [ValidatePattern("^[1-9][0-9]*$")]
  [string]$ExpectedRunnerRunAttempt,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedRunnerJob,

  [Parameter(Mandatory = $true)]
  [ValidatePattern("^sha256:[0-9a-f]{64}$")]
  [string]$ExpectedEvaExecutableSha256,

  [Parameter(Mandatory = $true)]
  [ValidateSet("Lifecycle", "Reboot")]
  [string]$Mode,

  [string]$ExpectedBootMarker = "",

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ReceiptPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$HarnessFormat = "eva.windows.platform_service_harness.v1"
$OwnerFormat = "eva.windows.platform_service_evidence.v1"
$CaptureFormat = "eva.release.command_capture.v1"
$ContinuationFormat = "eva.windows.platform_service_continuation.v1"
$ReceiptFormat = "eva.windows.platform_service_evidence_readback_receipt.v1"
$Utf8 = New-Object System.Text.UTF8Encoding($false, $true)

function Fail-Evidence {
  param([string]$Reason, [string]$Detail)
  $safe = if ([string]::IsNullOrWhiteSpace($Detail)) { "none" } else { $Detail.Replace("`r", " ").Replace("`n", " ") }
  throw "[platform-service-evidence] reason=$Reason detail=$safe"
}

function Get-FullPath {
  param([string]$Path)
  try {
    if ([string]::IsNullOrWhiteSpace($Path)) { Fail-Evidence "path_missing" "path" }
    if ([System.IO.Path]::IsPathRooted($Path)) { return [System.IO.Path]::GetFullPath($Path) }
    return [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $Path))
  } catch {
    if ($_.Exception.Message.StartsWith("[platform-service-evidence]", [System.StringComparison]::Ordinal)) { throw }
    Fail-Evidence "path_invalid" $Path
  }
}

function Get-Comparison {
  if ($env:OS -eq "Windows_NT") { return [System.StringComparison]::OrdinalIgnoreCase }
  return [System.StringComparison]::Ordinal
}

function Test-PathInside {
  param([string]$Child, [string]$Parent)
  $childFull = Get-FullPath $Child
  $parentFull = (Get-FullPath $Parent).TrimEnd([char[]]@('\', '/'))
  return $childFull.Equals($parentFull, (Get-Comparison)) -or $childFull.StartsWith("$parentFull$([System.IO.Path]::DirectorySeparatorChar)", (Get-Comparison))
}

function Assert-NoReparsePath {
  param([string]$Path, [string]$Field)
  $full = Get-FullPath $Path
  $root = [System.IO.Path]::GetPathRoot($full)
  $current = $root
  $relative = $full.Substring($root.Length)
  foreach ($part in $relative.Split([char[]]@('\', '/'), [System.StringSplitOptions]::RemoveEmptyEntries)) {
    $current = Join-Path $current $part
    if (-not (Test-Path -LiteralPath $current)) { break }
    if (([System.IO.File]::GetAttributes($current) -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
      Fail-Evidence "path_reparse_point" ("{0}:{1}" -f $Field, $current)
    }
  }
}

function Assert-NoReparseDescendants {
  param([string]$Path, [string]$Field)
  $full = Get-FullPath $Path
  Assert-NoReparsePath -Path $full -Field $Field
  if (-not (Test-Path -LiteralPath $full -PathType Container)) { Fail-Evidence "evidence_root_missing" $full }
  $pending = New-Object System.Collections.Generic.Queue[string]
  $pending.Enqueue($full)
  while ($pending.Count -gt 0) {
    $current = $pending.Dequeue()
    foreach ($entry in @(Get-ChildItem -LiteralPath $current -Force -ErrorAction Stop)) {
      if (($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        Fail-Evidence "path_reparse_point" ("{0}:{1}" -f $Field, $entry.FullName)
      }
      if ($entry.PSIsContainer) { $pending.Enqueue($entry.FullName) }
    }
  }
}

function Get-Sha256Bytes {
  param([byte[]]$Bytes)
  $sha = [System.Security.Cryptography.SHA256]::Create()
  try { return "sha256:$([System.BitConverter]::ToString($sha.ComputeHash($Bytes)).Replace('-', '').ToLowerInvariant())" } finally { $sha.Dispose() }
}

function Get-Sha256File {
  param([string]$Path)
  $full = Get-FullPath $Path
  Assert-NoReparsePath -Path $full -Field "digest_input"
  if (-not (Test-Path -LiteralPath $full -PathType Leaf)) { Fail-Evidence "file_missing" $full }
  return Get-Sha256Bytes ([System.IO.File]::ReadAllBytes($full))
}

function Read-Utf8 {
  param([string]$Path, [string]$Reason)
  $full = Get-FullPath $Path
  Assert-NoReparsePath -Path $full -Field $Reason
  if (-not (Test-Path -LiteralPath $full -PathType Leaf)) { Fail-Evidence $Reason $full }
  try { return $Utf8.GetString([System.IO.File]::ReadAllBytes($full)) } catch { Fail-Evidence "utf8_invalid" $full }
}

function Read-JsonFile {
  param([string]$Path, [string]$Reason)
  try { return (Read-Utf8 -Path $Path -Reason $Reason) | ConvertFrom-Json } catch {
    if ($_.Exception.Message.StartsWith("[platform-service-evidence]", [System.StringComparison]::Ordinal)) { throw }
    Fail-Evidence "json_invalid" $Path
  }
}

function Assert-ExactKeys {
  param([object]$Object, [string[]]$Keys, [string]$Reason, [string]$Detail)
  if ($null -eq $Object) { Fail-Evidence $Reason $Detail }
  $actual = @($Object.PSObject.Properties | ForEach-Object { $_.Name } | Sort-Object)
  $expected = @($Keys | Sort-Object)
  if (($actual -join "`n") -cne ($expected -join "`n")) {
    Fail-Evidence $Reason ("{0}: actual='{1}' expected='{2}'" -f $Detail, ($actual -join ","), ($expected -join ","))
  }
}

function Get-Prop {
  param([object]$Object, [string]$Name, [string]$Reason)
  if ($null -eq $Object -or $null -eq $Object.PSObject.Properties[$Name]) { Fail-Evidence $Reason $Name }
  return $Object.PSObject.Properties[$Name].Value
}

function Assert-Eq {
  param([object]$Actual, [object]$Expected, [string]$Reason, [string]$Detail)
  if ([string]$Actual -cne [string]$Expected) {
    Fail-Evidence $Reason ("{0}: actual='{1}' expected='{2}'" -f $Detail, [string]$Actual, [string]$Expected)
  }
}

function Assert-StringEq {
  param([object]$Actual, [string]$Expected, [string]$Reason, [string]$Detail)
  if (-not ($Actual -is [string]) -or [string]$Actual -cne $Expected) {
    Fail-Evidence $Reason ("{0}: actual='{1}' expected='{2}'" -f $Detail, [string]$Actual, $Expected)
  }
}

function Assert-Bool {
  param([object]$Actual, [bool]$Expected, [string]$Reason, [string]$Detail)
  if (-not ($Actual -is [bool]) -or [bool]$Actual -ne $Expected) {
    Fail-Evidence $Reason ("{0}: actual='{1}' expected='{2}'" -f $Detail, [string]$Actual, [string]$Expected)
  }
}

function Assert-Int {
  param([object]$Actual, [int64]$Expected, [string]$Reason, [string]$Detail)
  if (-not (($Actual -is [byte]) -or ($Actual -is [int16]) -or ($Actual -is [int]) -or ($Actual -is [long])) -or [int64]$Actual -ne $Expected) {
    Fail-Evidence $Reason ("{0}: actual='{1}' expected='{2}'" -f $Detail, [string]$Actual, [string]$Expected)
  }
}

function Assert-IntMin {
  param([object]$Actual, [int64]$Minimum, [string]$Reason, [string]$Detail)
  if (-not (($Actual -is [byte]) -or ($Actual -is [int16]) -or ($Actual -is [int]) -or ($Actual -is [long])) -or [int64]$Actual -lt $Minimum) {
    Fail-Evidence $Reason ("{0}: actual='{1}' minimum='{2}'" -f $Detail, [string]$Actual, [string]$Minimum)
  }
}

function Assert-Null {
  param([object]$Actual, [string]$Reason, [string]$Detail)
  if ($null -ne $Actual) { Fail-Evidence $Reason $Detail }
}

function Get-OriginalRelative {
  param([string]$OriginalRoot, [string]$OriginalPath, [string]$Field)
  $root = (Get-FullPath $OriginalRoot).TrimEnd([char[]]@('\', '/'))
  $path = Get-FullPath $OriginalPath
  if (-not ($path.Equals($root, (Get-Comparison)) -or $path.StartsWith("$root$([System.IO.Path]::DirectorySeparatorChar)", (Get-Comparison)))) {
    Fail-Evidence "path_escape" ("{0}:{1}" -f $Field, $OriginalPath)
  }
  return $path.Substring($root.Length).TrimStart([char[]]@('\', '/')).Replace('\', '/')
}

function Join-CheckedRelative {
  param([string]$Root, [string]$Relative, [string]$Field)
  if ([string]::IsNullOrWhiteSpace($Relative) -or [System.IO.Path]::IsPathRooted($Relative) -or $Relative -match '(^|[\\/])\.\.([\\/]|$)' -or $Relative.Contains([char]0)) {
    Fail-Evidence "path_escape" ("{0}:{1}" -f $Field, $Relative)
  }
  $full = Get-FullPath (Join-Path (Get-FullPath $Root) $Relative)
  if (-not (Test-PathInside -Child $full -Parent $Root)) { Fail-Evidence "path_escape" ("{0}:{1}" -f $Field, $Relative) }
  Assert-NoReparsePath -Path $full -Field $Field
  return $full
}

function Step {
  param([int]$Ordinal, [string]$Id, [string]$Command, [string]$State, [bool]$Mutation)
  return @{ ordinal = $Ordinal; step_id = $Id; command = $Command; state = $State; mutation = $Mutation }
}

function Get-LifecyclePlan {
  return @(
    (Step 0 "status-preflight" "status" "not_installed" $false),
    (Step 1 "install" "install" "stopped" $true),
    (Step 2 "install-idempotent" "install" "stopped" $false),
    (Step 3 "start" "start" "running" $true),
    (Step 4 "start-idempotent" "start" "running" $false),
    (Step 5 "status-running" "status" "running" $false),
    (Step 6 "restart" "restart" "running" $true),
    (Step 7 "status-post-restart" "status" "running" $false),
    (Step 8 "stop" "stop" "stopped" $true),
    (Step 9 "stop-idempotent" "stop" "stopped" $false),
    (Step 10 "uninstall" "uninstall" "not_installed" $true),
    (Step 11 "uninstall-idempotent" "uninstall" "not_installed" $false),
    (Step 12 "status-final" "status" "not_installed" $false)
  )
}

function Get-PreparePlan {
  $plan = @(Get-LifecyclePlan)
  return @($plan[0..5])
}

function Get-ResumePlan {
  return @(
    (Step 0 "status-resume-preflight" "status" "running" $false),
    (Step 1 "restart" "restart" "running" $true),
    (Step 2 "status-post-restart" "status" "running" $false),
    (Step 3 "stop" "stop" "stopped" $true),
    (Step 4 "stop-idempotent" "stop" "stopped" $false),
    (Step 5 "uninstall" "uninstall" "not_installed" $true),
    (Step 6 "uninstall-idempotent" "uninstall" "not_installed" $false),
    (Step 7 "status-final" "status" "not_installed" $false)
  )
}

function Assert-DigestSidecar {
  param([string]$JsonPath, [string]$DigestPath, [string]$Reason)
  $actual = Get-Sha256File $JsonPath
  $text = Read-Utf8 -Path $DigestPath -Reason $Reason
  if ($text.Length -ne 72 -or $text.Substring(71, 1) -cne "`n" -or $text.Substring(0, 71) -cnotmatch '^sha256:[0-9a-f]{64}$') {
    Fail-Evidence "digest_sidecar_invalid" $DigestPath
  }
  $expected = $text.Substring(0, 71)
  Assert-Eq $actual $expected "digest_mismatch" $JsonPath
  return $actual
}

function Assert-RootLayout {
  param([string]$Root, [string[]]$Files, [string[]]$Directories)
  $rootFull = Get-FullPath $Root
  $actualFiles = @(Get-ChildItem -LiteralPath $rootFull -Recurse -Force -File | ForEach-Object { $_.FullName.Substring($rootFull.Length).TrimStart([char[]]@('\', '/')).Replace('\', '/') } | Sort-Object)
  $actualDirs = @(Get-ChildItem -LiteralPath $rootFull -Recurse -Force -Directory | ForEach-Object { $_.FullName.Substring($rootFull.Length).TrimStart([char[]]@('\', '/')).Replace('\', '/') } | Sort-Object)
  $expectedFiles = @($Files | Sort-Object)
  $expectedDirs = @($Directories | Sort-Object)
  if (($actualFiles -join "`n") -cne ($expectedFiles -join "`n")) { Fail-Evidence "file_set_mismatch" "files" }
  if (($actualDirs -join "`n") -cne ($expectedDirs -join "`n")) { Fail-Evidence "file_set_mismatch" "directories" }
}

function Assert-Stream {
  param([object]$Stream, [string]$Directory, [string]$Name, [string]$Field)
  Assert-ExactKeys $Stream @("path", "byte_count", "sha256") "capture_stream_invalid" $Field
  Assert-StringEq (Get-Prop $Stream "path" "capture_stream_invalid") $Name "capture_stream_path_invalid" $Field
  $path = Join-CheckedRelative -Root $Directory -Relative $Name -Field $Field
  if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { Fail-Evidence "capture_stream_missing" $path }
  $bytes = [System.IO.File]::ReadAllBytes($path)
  Assert-Int (Get-Prop $Stream "byte_count" "capture_stream_invalid") ([int64]$bytes.Length) "capture_stream_size_mismatch" $Field
  Assert-StringEq (Get-Prop $Stream "sha256" "capture_stream_invalid") (Get-Sha256Bytes $bytes) "capture_stream_digest_mismatch" $Field
  return [pscustomobject]@{ Path = $path; Text = $Utf8.GetString($bytes); ByteCount = [int64]$bytes.Length }
}

function Assert-Runner {
  param([object]$Runner)
  Assert-ExactKeys $Runner @("provider", "identity", "name", "os", "architecture", "run_id", "run_attempt", "job") "capture_runner_invalid" "runner"
  Assert-StringEq (Get-Prop $Runner "provider" "capture_runner_invalid") $ExpectedRunnerProvider "capture_runner_invalid" "provider"
  Assert-StringEq (Get-Prop $Runner "identity" "capture_runner_invalid") $ExpectedRunnerIdentity "capture_runner_invalid" "identity"
  Assert-StringEq (Get-Prop $Runner "run_id" "capture_runner_invalid") $ExpectedRunnerRunId "capture_runner_invalid" "run_id"
  Assert-StringEq (Get-Prop $Runner "run_attempt" "capture_runner_invalid") $ExpectedRunnerRunAttempt "capture_runner_invalid" "run_attempt"
  Assert-StringEq (Get-Prop $Runner "job" "capture_runner_invalid") $ExpectedRunnerJob "capture_runner_invalid" "job"
  Assert-StringEq (Get-Prop $Runner "os" "capture_runner_invalid") "Windows" "capture_runner_invalid" "os"
  if ([string]::IsNullOrWhiteSpace([string](Get-Prop $Runner "name" "capture_runner_invalid")) -or [string]::IsNullOrWhiteSpace([string](Get-Prop $Runner "architecture" "capture_runner_invalid"))) {
    Fail-Evidence "capture_runner_invalid" "name_or_architecture"
  }
}

function Assert-Stdout {
  param([string]$Text, [hashtable]$Plan, [string]$ServiceName)
  try { $response = $Text | ConvertFrom-Json } catch { Fail-Evidence "capture_stdout_json_invalid" $Plan.step_id }
  Assert-ExactKeys $response @("ok", "command", "exit_code", "data", "trace") "capture_stdout_contract_invalid" "$($Plan.step_id):stdout"
  Assert-Bool (Get-Prop $response "ok" "capture_stdout_contract_invalid") $true "capture_stdout_contract_invalid" "$($Plan.step_id):ok"
  Assert-Int (Get-Prop $response "exit_code" "capture_stdout_contract_invalid") 0 "capture_stdout_contract_invalid" "$($Plan.step_id):exit_code"
  Assert-StringEq (Get-Prop $response "command" "capture_stdout_contract_invalid") "service.$($Plan.command)" "capture_stdout_contract_invalid" "$($Plan.step_id):command"
  $trace = Get-Prop $response "trace" "capture_stdout_contract_invalid"
  Assert-ExactKeys $trace @("span_id") "capture_stdout_contract_invalid" "$($Plan.step_id):trace"
  Assert-StringEq (Get-Prop $trace "span_id" "capture_stdout_contract_invalid") "cli.service.$($Plan.command)" "capture_stdout_contract_invalid" "$($Plan.step_id):trace.span_id"
  $data = Get-Prop $response "data" "capture_stdout_contract_invalid"
  if ([string]$Plan.command -ceq "status") {
    Assert-ExactKeys $data @("kind", "service_name", "configured", "production_adapter", "state", "mutation_executed", "active_generation", "active_release", "candidate_generation", "audit") "capture_stdout_contract_invalid" "$($Plan.step_id):data"
    Assert-Bool (Get-Prop $data "configured" "capture_stdout_contract_invalid") $true "capture_stdout_contract_invalid" "$($Plan.step_id):configured"
    Assert-Null (Get-Prop $data "active_generation" "capture_stdout_contract_invalid") "capture_stdout_contract_invalid" "$($Plan.step_id):active_generation"
    Assert-Null (Get-Prop $data "active_release" "capture_stdout_contract_invalid") "capture_stdout_contract_invalid" "$($Plan.step_id):active_release"
    Assert-Null (Get-Prop $data "candidate_generation" "capture_stdout_contract_invalid") "capture_stdout_contract_invalid" "$($Plan.step_id):candidate_generation"
  } else {
    Assert-ExactKeys $data @("kind", "service_name", "operation", "state", "mutation_executed", "production_adapter", "audit") "capture_stdout_contract_invalid" "$($Plan.step_id):data"
    Assert-StringEq (Get-Prop $data "operation" "capture_stdout_contract_invalid") ([string]$Plan.command) "capture_stdout_contract_invalid" "$($Plan.step_id):operation"
  }
  Assert-StringEq (Get-Prop $data "kind" "capture_stdout_contract_invalid") "windows_service" "capture_stdout_contract_invalid" "$($Plan.step_id):kind"
  Assert-StringEq (Get-Prop $data "service_name" "capture_stdout_contract_invalid") $ServiceName "capture_stdout_contract_invalid" "$($Plan.step_id):service_name"
  Assert-Bool (Get-Prop $data "production_adapter" "capture_stdout_contract_invalid") $true "capture_stdout_contract_invalid" "$($Plan.step_id):production_adapter"
  Assert-StringEq (Get-Prop $data "state" "capture_stdout_contract_invalid") ([string]$Plan.state) "capture_stdout_contract_invalid" "$($Plan.step_id):state"
  Assert-Bool (Get-Prop $data "mutation_executed" "capture_stdout_contract_invalid") ([bool]$Plan.mutation) "capture_stdout_contract_invalid" "$($Plan.step_id):mutation"
  if ($null -eq (Get-Prop $data "audit" "capture_stdout_contract_invalid")) {
    Fail-Evidence "capture_stdout_contract_invalid" "$($Plan.step_id):audit"
  }
}

function Assert-Owner {
  param([object]$Owner, [string]$ServiceName)
  Assert-ExactKeys $Owner @("format", "source_commit", "run_id", "service_name", "repository_root", "project_root", "eva_executable_sha256") "owner_invalid" "owner"
  Assert-StringEq (Get-Prop $Owner "format" "owner_invalid") $OwnerFormat "owner_invalid" "format"
  Assert-StringEq (Get-Prop $Owner "source_commit" "owner_invalid") $ExpectedSourceCommit "owner_invalid" "source_commit"
  Assert-StringEq (Get-Prop $Owner "run_id" "owner_invalid") $ExpectedRunId "owner_invalid" "run_id"
  Assert-StringEq (Get-Prop $Owner "service_name" "owner_invalid") $ServiceName "owner_invalid" "service_name"
  Assert-StringEq (Get-Prop $Owner "repository_root" "owner_invalid") $ExpectedRepositoryRoot "owner_invalid" "repository_root"
  Assert-StringEq (Get-Prop $Owner "eva_executable_sha256" "owner_invalid") $ExpectedEvaExecutableSha256 "owner_invalid" "eva_executable_sha256"
}

function Assert-Authority {
  param([object]$Authority)
  Assert-ExactKeys $Authority @("allowed", "reasons", "is_windows", "is_admin", "execute", "controlled_host") "authority_invalid" "authority"
  foreach ($name in @("allowed", "is_windows", "is_admin", "execute", "controlled_host")) {
    Assert-Bool (Get-Prop $Authority $name "authority_invalid") $true "authority_invalid" $name
  }
  Assert-Eq (@(Get-Prop $Authority "reasons" "authority_invalid").Count) 0 "authority_invalid" "reasons"
}

function Assert-Transcript {
  param([string]$Root, [object]$Owner, [object]$Transcript, [string]$Digest, [string]$ExpectedMode, [string]$ExpectedStatus, [hashtable[]]$Plan)
  $serviceName = "eva-ext01-$ExpectedRunId"
  Assert-Owner -Owner $Owner -ServiceName $serviceName
  Assert-ExactKeys $Transcript @("format", "mode", "status", "source_commit", "run_id", "service_name", "project_root", "evidence_root", "eva_executable", "eva_executable_sha256", "authority", "steps", "continuation", "warnings", "written_at") "transcript_invalid" $ExpectedMode
  Assert-StringEq (Get-Prop $Transcript "format" "transcript_invalid") $HarnessFormat "transcript_invalid" "format"
  Assert-StringEq (Get-Prop $Transcript "mode" "transcript_invalid") $ExpectedMode "transcript_mode_invalid" "mode"
  Assert-StringEq (Get-Prop $Transcript "status" "transcript_invalid") $ExpectedStatus "transcript_status_invalid" "status"
  Assert-StringEq (Get-Prop $Transcript "source_commit" "transcript_invalid") $ExpectedSourceCommit "transcript_subject_mismatch" "source_commit"
  Assert-StringEq (Get-Prop $Transcript "run_id" "transcript_invalid") $ExpectedRunId "transcript_subject_mismatch" "run_id"
  Assert-StringEq (Get-Prop $Transcript "service_name" "transcript_invalid") $serviceName "transcript_subject_mismatch" "service_name"
  Assert-StringEq (Get-Prop $Transcript "eva_executable_sha256" "transcript_invalid") $ExpectedEvaExecutableSha256 "transcript_subject_mismatch" "eva_executable_sha256"
  Assert-StringEq (Get-Prop $Owner "project_root" "owner_invalid") ([string](Get-Prop $Transcript "project_root" "transcript_invalid")) "owner_invalid" "project_root"
  Assert-Authority (Get-Prop $Transcript "authority" "authority_invalid")
  Assert-Eq (@(Get-Prop $Transcript "warnings" "transcript_invalid").Count) 0 "transcript_warnings_invalid" "warnings"
  $originalRoot = [string](Get-Prop $Transcript "evidence_root" "transcript_invalid")
  $steps = @(Get-Prop $Transcript "steps" "transcript_invalid")
  Assert-Eq $steps.Count $Plan.Count "step_count_invalid" "steps"
  for ($i = 0; $i -lt $Plan.Count; $i += 1) {
    $p = $Plan[$i]
    $s = $steps[$i]
    Assert-ExactKeys $s @("ordinal", "step_id", "command", "expected_state", "actual_state", "mutation_executed", "capture_path", "stdout_path") "step_contract_invalid" "$($p.step_id):fields"
    Assert-Int (Get-Prop $s "ordinal" "step_contract_invalid") ([int64]$p.ordinal) "step_contract_invalid" "$($p.step_id):ordinal"
    Assert-StringEq (Get-Prop $s "step_id" "step_contract_invalid") ([string]$p.step_id) "step_contract_invalid" "$($p.step_id):step_id"
    Assert-StringEq (Get-Prop $s "command" "step_contract_invalid") "service.$($p.command)" "step_contract_invalid" "$($p.step_id):command"
    Assert-StringEq (Get-Prop $s "expected_state" "step_contract_invalid") ([string]$p.state) "step_contract_invalid" "$($p.step_id):expected_state"
    Assert-StringEq (Get-Prop $s "actual_state" "step_contract_invalid") ([string]$p.state) "step_contract_invalid" "$($p.step_id):actual_state"
    Assert-Bool (Get-Prop $s "mutation_executed" "step_contract_invalid") ([bool]$p.mutation) "step_contract_invalid" "$($p.step_id):mutation"
    $dirRel = "captures/{0:D2}-{1}" -f [int]$p.ordinal, [string]$p.step_id
    $captureRel = Get-OriginalRelative -OriginalRoot $originalRoot -OriginalPath ([string](Get-Prop $s "capture_path" "step_contract_invalid")) -Field "$($p.step_id):capture_path"
    Assert-StringEq $captureRel "$dirRel/capture.json" "step_capture_path_invalid" "$($p.step_id):capture_path"
    $stdoutRel = Get-OriginalRelative -OriginalRoot $originalRoot -OriginalPath ([string](Get-Prop $s "stdout_path" "step_contract_invalid")) -Field "$($p.step_id):stdout_path"
    Assert-StringEq $stdoutRel "$dirRel/capture.stdout" "step_capture_path_invalid" "$($p.step_id):stdout_path"
    $capturePath = Join-CheckedRelative -Root $Root -Relative $captureRel -Field "$($p.step_id):capture_path"
    $captureDir = [System.IO.Path]::GetDirectoryName($capturePath)
    $capture = Read-JsonFile -Path $capturePath -Reason "capture_missing"
    Assert-ExactKeys $capture @("format", "capture_id", "executable", "argv", "outcome", "started_at", "finished_at", "duration_ms", "exit_code", "failure_reason", "runner", "stdout", "stderr") "capture_invalid" "$($p.step_id):fields"
    Assert-StringEq (Get-Prop $capture "format" "capture_invalid") $CaptureFormat "capture_invalid" "$($p.step_id):format"
    Assert-StringEq (Get-Prop $capture "capture_id" "capture_invalid") "platform-service.$($p.step_id)" "capture_invalid" "$($p.step_id):capture_id"
    Assert-StringEq (Get-Prop $capture "executable" "capture_invalid") ([string](Get-Prop $Transcript "eva_executable" "transcript_invalid")) "capture_invalid" "$($p.step_id):executable"
    Assert-StringEq (Get-Prop $capture "outcome" "capture_invalid") "success" "capture_outcome_invalid" "$($p.step_id):outcome"
    Assert-Int (Get-Prop $capture "exit_code" "capture_outcome_invalid") 0 "capture_outcome_invalid" "$($p.step_id):exit_code"
    Assert-IntMin (Get-Prop $capture "duration_ms" "capture_outcome_invalid") 0 "capture_outcome_invalid" "$($p.step_id):duration_ms"
    Assert-Null (Get-Prop $capture "failure_reason" "capture_outcome_invalid") "capture_outcome_invalid" "$($p.step_id):failure_reason"
    $argv = @(Get-Prop $capture "argv" "capture_argv_invalid")
    $expectedArgv = @("service", [string]$p.command, "--project", [string](Get-Prop $Transcript "project_root" "transcript_invalid"), "--output", "json")
    Assert-Eq $argv.Count $expectedArgv.Count "capture_argv_invalid" "$($p.step_id):argv_count"
    for ($j = 0; $j -lt $expectedArgv.Count; $j += 1) { Assert-StringEq $argv[$j] $expectedArgv[$j] "capture_argv_invalid" "$($p.step_id):argv[$j]" }
    Assert-Runner (Get-Prop $capture "runner" "capture_runner_invalid")
    $stdout = Assert-Stream -Stream (Get-Prop $capture "stdout" "capture_stream_invalid") -Directory $captureDir -Name "capture.stdout" -Field "$($p.step_id):stdout"
    $stderr = Assert-Stream -Stream (Get-Prop $capture "stderr" "capture_stream_invalid") -Directory $captureDir -Name "capture.stderr" -Field "$($p.step_id):stderr"
    Assert-Eq $stderr.ByteCount 0 "capture_stderr_nonempty" "$($p.step_id):stderr"
    Assert-Eq $stderr.Text "" "capture_stderr_nonempty" "$($p.step_id):stderr"
    Assert-Stdout -Text $stdout.Text -Plan $p -ServiceName $serviceName
  }
  return [pscustomobject]@{ ServiceName = $serviceName; TranscriptDigest = $Digest; CaptureCount = $Plan.Count }
}

function Add-CaptureLayout {
  param([System.Collections.Generic.List[string]]$Files, [System.Collections.Generic.List[string]]$Directories, [hashtable[]]$Plan)
  foreach ($p in $Plan) {
    $dir = "captures/{0:D2}-{1}" -f [int]$p.ordinal, [string]$p.step_id
    $Directories.Add($dir)
    $Files.Add("$dir/capture.json")
    $Files.Add("$dir/capture.stdout")
    $Files.Add("$dir/capture.stderr")
  }
}

function Read-TranscriptBundle {
  param([string]$Root, [string]$Stem)
  $json = Join-Path $Root "$Stem.json"
  $digest = Assert-DigestSidecar -JsonPath $json -DigestPath (Join-Path $Root "$Stem.sha256") -Reason "${Stem}_digest_missing"
  return [pscustomobject]@{ Transcript = Read-JsonFile -Path $json -Reason "${Stem}_missing"; Digest = $digest }
}

function Read-ContinuationBundle {
  param([string]$Root)
  $json = Join-Path $Root "continuation.json"
  $digest = Assert-DigestSidecar -JsonPath $json -DigestPath (Join-Path $Root "continuation.sha256") -Reason "continuation_digest_missing"
  $manifest = Read-JsonFile -Path $json -Reason "continuation_missing"
  Assert-ExactKeys $manifest @("format", "source_commit", "run_id", "service_name", "project_root", "evidence_root", "eva_executable", "eva_executable_sha256", "prepared_boot_marker", "expected_boot_marker", "prepared_at") "continuation_invalid" "fields"
  Assert-StringEq (Get-Prop $manifest "format" "continuation_invalid") $ContinuationFormat "continuation_invalid" "format"
  Assert-StringEq (Get-Prop $manifest "source_commit" "continuation_invalid") $ExpectedSourceCommit "continuation_invalid" "source_commit"
  Assert-StringEq (Get-Prop $manifest "run_id" "continuation_invalid") $ExpectedRunId "continuation_invalid" "run_id"
  Assert-StringEq (Get-Prop $manifest "service_name" "continuation_invalid") "eva-ext01-$ExpectedRunId" "continuation_invalid" "service_name"
  Assert-StringEq (Get-Prop $manifest "eva_executable_sha256" "continuation_invalid") $ExpectedEvaExecutableSha256 "continuation_invalid" "eva_executable_sha256"
  if ([string]::IsNullOrWhiteSpace([string](Get-Prop $manifest "prepared_boot_marker" "continuation_invalid"))) { Fail-Evidence "continuation_invalid" "prepared_boot_marker" }
  Assert-StringEq (Get-Prop $manifest "expected_boot_marker" "continuation_invalid") $ExpectedBootMarker "continuation_invalid" "expected_boot_marker"
  return [pscustomobject]@{ Manifest = $manifest; Digest = $digest }
}

function Assert-PrepareContinuation {
  param([object]$Transcript, [object]$Continuation)
  $c = Get-Prop $Transcript "continuation" "continuation_invalid"
  Assert-ExactKeys $c @("manifest", "digest", "path", "digest_path") "continuation_invalid" "prepare"
  Assert-StringEq (Get-Prop $c "digest" "continuation_invalid") ([string]$Continuation.Digest) "continuation_invalid" "prepare.digest"
  Assert-StringEq (Get-Prop $c "path" "continuation_invalid") (Join-Path ([string](Get-Prop $Transcript "evidence_root" "transcript_invalid")) "continuation.json") "continuation_invalid" "prepare.path"
  Assert-StringEq (Get-Prop $c "digest_path" "continuation_invalid") (Join-Path ([string](Get-Prop $Transcript "evidence_root" "transcript_invalid")) "continuation.sha256") "continuation_invalid" "prepare.digest_path"
  $embedded = Get-Prop $c "manifest" "continuation_invalid"
  Assert-ExactKeys $embedded @("format", "source_commit", "run_id", "service_name", "project_root", "evidence_root", "eva_executable", "eva_executable_sha256", "prepared_boot_marker", "expected_boot_marker", "prepared_at") "continuation_invalid" "prepare.manifest"
  foreach ($name in @("format", "source_commit", "run_id", "service_name", "project_root", "evidence_root", "eva_executable", "eva_executable_sha256", "prepared_boot_marker", "expected_boot_marker", "prepared_at")) {
    Assert-Eq (Get-Prop $embedded $name "continuation_invalid") (Get-Prop $Continuation.Manifest $name "continuation_invalid") "continuation_invalid" "prepare.manifest.$name"
  }
}

function Assert-ResumeContinuation {
  param([object]$Transcript, [object]$Continuation)
  $c = Get-Prop $Transcript "continuation" "continuation_invalid"
  Assert-ExactKeys $c @("digest", "current_boot_marker") "continuation_invalid" "resume"
  Assert-StringEq (Get-Prop $c "digest" "continuation_invalid") ([string]$Continuation.Digest) "continuation_invalid" "resume.digest"
  $current = [string](Get-Prop $c "current_boot_marker" "continuation_invalid")
  if ([string]::IsNullOrWhiteSpace($current)) { Fail-Evidence "continuation_invalid" "resume.current_boot_marker" }
  if ($current -ceq [string](Get-Prop $Continuation.Manifest "prepared_boot_marker" "continuation_invalid")) { Fail-Evidence "continuation_boot_marker_unchanged" $current }
  $expected = [string](Get-Prop $Continuation.Manifest "expected_boot_marker" "continuation_invalid")
  if (-not [string]::IsNullOrWhiteSpace($expected) -and $current -cne $expected) { Fail-Evidence "continuation_boot_marker_mismatch" ("actual='{0}' expected='{1}'" -f $current, $expected) }
}

function Write-NewJson {
  param([string]$Path, [object]$Value)
  $full = Get-FullPath $Path
  Assert-NoReparsePath -Path $full -Field "receipt_path"
  $parent = [System.IO.Path]::GetDirectoryName($full)
  if ([string]::IsNullOrWhiteSpace($parent)) { Fail-Evidence "receipt_path_invalid" $full }
  Assert-NoReparsePath -Path $parent -Field "receipt_parent"
  [System.IO.Directory]::CreateDirectory($parent) | Out-Null
  try {
    $stream = New-Object System.IO.FileStream($full, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    try {
      $json = (($Value | ConvertTo-Json -Depth 16 -Compress).Replace("`r`n", "`n").Replace("`r", "`n"))
      $bytes = $Utf8.GetBytes("$json`n")
      $stream.Write($bytes, 0, $bytes.Length)
      $stream.Flush($true)
    } finally { $stream.Dispose() }
  } catch { Fail-Evidence "receipt_exists" $full }
}

$root = Get-FullPath $EvidencePath
$receiptFull = Get-FullPath $ReceiptPath
Assert-NoReparseDescendants -Path $root -Field "evidence_root"
if (Test-PathInside -Child $receiptFull -Parent $root) { Fail-Evidence "receipt_inside_evidence" $receiptFull }
Assert-NoReparsePath -Path $receiptFull -Field "receipt_path"
if (Test-Path -LiteralPath $receiptFull) { Fail-Evidence "receipt_exists" $receiptFull }

$owner = Read-JsonFile -Path (Join-Path $root "platform-service-harness.owner.json") -Reason "owner_missing"
$files = New-Object System.Collections.Generic.List[string]
$dirs = New-Object System.Collections.Generic.List[string]
$files.Add("platform-service-harness.owner.json")
$dirs.Add("captures")
$digests = [ordered]@{}

if ($Mode -eq "Lifecycle") {
  $plan = @(Get-LifecyclePlan)
  $files.Add("transcript.lifecycle.json")
  $files.Add("transcript.lifecycle.sha256")
  Add-CaptureLayout -Files $files -Directories $dirs -Plan $plan
  $bundle = Read-TranscriptBundle -Root $root -Stem "transcript.lifecycle"
  Assert-Null (Get-Prop $bundle.Transcript "continuation" "transcript_invalid") "transcript_continuation_invalid" "lifecycle"
  $result = Assert-Transcript -Root $root -Owner $owner -Transcript $bundle.Transcript -Digest $bundle.Digest -ExpectedMode "Lifecycle" -ExpectedStatus "success" -Plan $plan
  $digests["lifecycle"] = $bundle.Digest
} else {
  if ([string]::IsNullOrWhiteSpace($ExpectedBootMarker)) { Fail-Evidence "expected_boot_marker_required" "Reboot" }
  $preparePlan = @(Get-PreparePlan)
  $resumePlan = @(Get-ResumePlan)
  foreach ($name in @("continuation.json", "continuation.sha256", "transcript.preparereboot.json", "transcript.preparereboot.sha256", "transcript.resumereboot.json", "transcript.resumereboot.sha256")) { $files.Add($name) }
  Add-CaptureLayout -Files $files -Directories $dirs -Plan $preparePlan
  Add-CaptureLayout -Files $files -Directories $dirs -Plan $resumePlan
  $continuation = Read-ContinuationBundle -Root $root
  $prepare = Read-TranscriptBundle -Root $root -Stem "transcript.preparereboot"
  $resume = Read-TranscriptBundle -Root $root -Stem "transcript.resumereboot"
  Assert-Eq (Get-Prop $prepare.Transcript "project_root" "transcript_invalid") (Get-Prop $continuation.Manifest "project_root" "continuation_invalid") "continuation_invalid" "project_root"
  Assert-Eq (Get-Prop $prepare.Transcript "evidence_root" "transcript_invalid") (Get-Prop $continuation.Manifest "evidence_root" "continuation_invalid") "continuation_invalid" "evidence_root"
  Assert-Eq (Get-Prop $prepare.Transcript "eva_executable" "transcript_invalid") (Get-Prop $continuation.Manifest "eva_executable" "continuation_invalid") "continuation_invalid" "eva_executable"
  Assert-Eq (Get-Prop $resume.Transcript "project_root" "transcript_invalid") (Get-Prop $continuation.Manifest "project_root" "continuation_invalid") "continuation_invalid" "resume.project_root"
  Assert-Eq (Get-Prop $resume.Transcript "evidence_root" "transcript_invalid") (Get-Prop $continuation.Manifest "evidence_root" "continuation_invalid") "continuation_invalid" "resume.evidence_root"
  Assert-Eq (Get-Prop $resume.Transcript "eva_executable" "transcript_invalid") (Get-Prop $continuation.Manifest "eva_executable" "continuation_invalid") "continuation_invalid" "resume.eva_executable"
  Assert-PrepareContinuation -Transcript $prepare.Transcript -Continuation $continuation
  Assert-ResumeContinuation -Transcript $resume.Transcript -Continuation $continuation
  $prepareResult = Assert-Transcript -Root $root -Owner $owner -Transcript $prepare.Transcript -Digest $prepare.Digest -ExpectedMode "PrepareReboot" -ExpectedStatus "continuation_ready" -Plan $preparePlan
  $resumeResult = Assert-Transcript -Root $root -Owner $owner -Transcript $resume.Transcript -Digest $resume.Digest -ExpectedMode "ResumeReboot" -ExpectedStatus "success" -Plan $resumePlan
  $result = [pscustomobject]@{ ServiceName = $resumeResult.ServiceName; TranscriptDigest = $resumeResult.TranscriptDigest; CaptureCount = $prepareResult.CaptureCount + $resumeResult.CaptureCount }
  $digests["preparereboot"] = $prepare.Digest
  $digests["resumereboot"] = $resume.Digest
}

Assert-RootLayout -Root $root -Files @($files) -Directories @($dirs)
$treeLines = @(Get-ChildItem -LiteralPath $root -Recurse -Force -File | ForEach-Object {
    $relative = $_.FullName.Substring($root.Length).TrimStart([char[]]@('\', '/')).Replace('\', '/')
    "$relative $((Get-Sha256File $_.FullName))"
  } | Sort-Object)
$treeDigest = Get-Sha256Bytes ($Utf8.GetBytes(($treeLines -join "`n") + "`n"))

$receipt = [ordered]@{
  schema = $ReceiptFormat
  status = "verified_local_readback"
  mode = $Mode
  source_commit = $ExpectedSourceCommit
  repository_root = $ExpectedRepositoryRoot
  run_id = $ExpectedRunId
  service_name = $result.ServiceName
  runner = [ordered]@{
    provider = $ExpectedRunnerProvider
    identity = $ExpectedRunnerIdentity
    run_id = $ExpectedRunnerRunId
    run_attempt = $ExpectedRunnerRunAttempt
    job = $ExpectedRunnerJob
    os = "Windows"
  }
  eva_executable_sha256 = $ExpectedEvaExecutableSha256
  evidence_root = $root
  transcript_digests = $digests
  capture_count = $result.CaptureCount
  tree_digest = $treeDigest
  verified_at = [System.DateTimeOffset]::UtcNow.ToString("o", [System.Globalization.CultureInfo]::InvariantCulture)
}

Write-NewJson -Path $receiptFull -Value $receipt
Write-Output $receiptFull
