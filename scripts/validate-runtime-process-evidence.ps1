[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [Alias("EvidenceDirectory", "Path")]
  [string]$EvidencePath,

  [Parameter(Mandatory = $true)]
  [string]$ExpectedSourceCommit,

  [Parameter(Mandatory = $true)]
  [string]$ExpectedRunId,

  [Parameter(Mandatory = $true)]
  [string]$ExpectedRunAttempt
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$SubjectFormat = "eva.runtime-process.subject.v1"
$EnvelopeFormat = "eva.release.evidence_envelope.v1"
$CaptureFormat = "eva.release.command_capture.v1"
$RequiredPlatforms = @("windows", "linux", "macos")
$RequiredScenarios = @("background_owner", "forced_kill", "stale_lock_reclaim", "restart_recovery", "effect_dedup")

function Fail-Evidence([string]$Reason, [string]$Detail) {
  $safe = if ([string]::IsNullOrWhiteSpace($Detail)) { "none" } else { $Detail.Replace("`r", " ").Replace("`n", " ") }
  throw "[runtime-process-evidence] reason=$Reason detail=$safe"
}

function Assert-TrustedInputs {
  if ($ExpectedSourceCommit -cnotmatch '^[0-9a-f]{40}$') { Fail-Evidence "source_commit_invalid" $ExpectedSourceCommit }
  if ($ExpectedRunId -notmatch '^[1-9][0-9]*$') { Fail-Evidence "run_id_invalid" $ExpectedRunId }
  if ($ExpectedRunAttempt -notmatch '^[1-9][0-9]*$') { Fail-Evidence "run_attempt_invalid" $ExpectedRunAttempt }
}

function Get-FullPath([string]$Path) {
  try { return [IO.Path]::GetFullPath($Path) } catch { Fail-Evidence "path_invalid" $Path }
}

function Get-Comparison { if ($env:OS -eq "Windows_NT") { return [StringComparison]::OrdinalIgnoreCase }; return [StringComparison]::Ordinal }

function Assert-Directory([string]$Path, [string]$Label) {
  $full = Get-FullPath $Path
  if (-not [IO.Directory]::Exists($full)) { Fail-Evidence "directory_missing" $Label }
  if (([IO.File]::GetAttributes($full) -band [IO.FileAttributes]::ReparsePoint) -ne 0) { Fail-Evidence "path_symlink" $Label }
  return $full
}

function Get-SafeFile([string]$Root, [string]$Relative, [string]$Label) {
  if ($Relative -match '(^|[\\/])\.\.([\\/]|$)' -or [IO.Path]::IsPathRooted($Relative)) { Fail-Evidence "path_invalid" $Label }
  $rootFull = Assert-Directory $Root "evidence root"
  $full = Get-FullPath (Join-Path $rootFull $Relative)
  $prefix = $rootFull.TrimEnd([char[]]@('/', '\')) + [IO.Path]::DirectorySeparatorChar
  if (-not $full.StartsWith($prefix, (Get-Comparison))) { Fail-Evidence "path_escape" $Label }
  $current = $rootFull
  foreach ($part in $Relative.Replace('\', '/').Split('/')) {
    $current = Join-Path $current $part
    if (-not [IO.File]::Exists($current) -and -not [IO.Directory]::Exists($current)) { Fail-Evidence "file_missing" $Label }
    if (([IO.File]::GetAttributes($current) -band [IO.FileAttributes]::ReparsePoint) -ne 0) { Fail-Evidence "path_symlink" $Label }
  }
  if (-not [IO.File]::Exists($full)) { Fail-Evidence "file_not_regular" $Label }
  return $full
}

function Get-Sha256([byte[]]$Bytes) {
  $sha = [Security.Cryptography.SHA256]::Create()
  try { return "sha256:$([BitConverter]::ToString($sha.ComputeHash($Bytes)).Replace('-', '').ToLowerInvariant())" } finally { $sha.Dispose() }
}

function Parse-Manifest([string]$Path, [string]$Label) {
  [byte[]]$bytes = [IO.File]::ReadAllBytes($Path)
  try { $text = $Utf8NoBom.GetString($bytes) } catch { Fail-Evidence "manifest_not_utf8" $Label }
  if (-not $text.EndsWith("`n") -or $text.Contains("`r")) { Fail-Evidence "manifest_not_canonical" $Label }
  $map = @{}
  foreach ($line in $text.TrimEnd("`n").Split("`n")) {
    $at = $line.IndexOf('=')
    if ($at -le 0) { Fail-Evidence "manifest_line_invalid" $Label }
    $key = $line.Substring(0, $at); $value = $line.Substring($at + 1)
    if ($key -notmatch '^[a-z][a-z0-9_.]*$' -or $value.Length -eq 0 -or $value -match '[\r\n]') { Fail-Evidence "manifest_field_invalid" $Label }
    if ($map.ContainsKey($key)) { Fail-Evidence "manifest_field_duplicate" "${Label}:$key" }
    $map[$key] = $value
  }
  return $map
}

function Require-ExactKeys([hashtable]$Map, [string[]]$Keys, [string]$Label) {
  foreach ($key in $Keys) { if (-not $Map.ContainsKey($key)) { Fail-Evidence "manifest_field_missing" "${Label}:$key" } }
  foreach ($key in $Map.Keys) { if ($Keys -notcontains $key) { Fail-Evidence "manifest_field_unknown" "${Label}:$key" } }
}

function Get-Int([string]$Value, [string]$Label, [int64]$Minimum = 0) {
  [int64]$number = 0
  if (-not [Int64]::TryParse($Value, [ref]$number) -or $number -lt $Minimum -or $Value -ne $number.ToString([Globalization.CultureInfo]::InvariantCulture)) { Fail-Evidence "integer_invalid" $Label }
  return $number
}

function Assert-Facts([string]$Id, [hashtable]$Facts, [string]$Platform) {
  $required = @{}
  switch ($Id) {
    "background_owner" { $required = @{ child_pid_positive = "true"; controlled_shutdown = "true"; released = "true" } }
    "forced_kill" { $required = @{ termination = "forced"; owner_dead = "true"; stale_pid_preserved = "true"; mechanism = $(if ($Platform -eq "windows") { "terminate_process" } else { "sigkill" }) } }
    "stale_lock_reclaim" { $required = @{ immediate_restart_blocked = "true"; new_generation_gt_old = "true"; old_generation = $null; new_generation = $null } }
    "restart_recovery" { $required = @{ status = "completed"; attempts_min = "2"; generation_increased = "true"; result_digest_present = "true" } }
    "effect_dedup" { $required = @{ applied_count = "1"; committed_applied_count = "1"; prepared_applied_count = "1"; duplicate = "false"; status = "interrupted"; operator_block = "true"; committed_reused = "true"; committed_status = "completed"; prepared_attempts = "1"; stable_restart = "true" } }
  }
  foreach ($key in $required.Keys) {
    if (-not $Facts.ContainsKey($key)) { Fail-Evidence "scenario_fact_missing" "${Id}:$key" }
    if ($key -eq "attempts_min") {
      if ((Get-Int $Facts[$key] "${Id}:$key" 0) -lt 2) { Fail-Evidence "scenario_fact_invalid" "${Id}:$key" }
    } elseif ($null -ne $required[$key] -and $Facts[$key] -cne $required[$key]) { Fail-Evidence "scenario_fact_invalid" "${Id}:$key" }
  }
  if ($Id -eq "stale_lock_reclaim") {
    $oldGeneration = Get-Int $Facts.old_generation "${Id}:old_generation" 1
    $newGeneration = Get-Int $Facts.new_generation "${Id}:new_generation" 1
    if ($newGeneration -le $oldGeneration) { Fail-Evidence "scenario_fact_invalid" "${Id}:generation_order" }
  }
}

function Validate-Platform([string]$Root, [string]$ExpectedOs) {
  $root = Assert-Directory $Root "platform root"
  $requiredFiles = @("runtime-process.subject", "runtime-process.envelope", "suite.capture.json", "suite.stdout", "suite.stderr")
  $platformEntries = @(Get-ChildItem -LiteralPath $root -Force)
  if ($platformEntries.Count -ne $requiredFiles.Count) { Fail-Evidence "platform_file_set_invalid" $ExpectedOs }
  foreach ($entry in $platformEntries) {
    if ($entry.PSIsContainer -or $requiredFiles -notcontains $entry.Name) { Fail-Evidence "platform_file_set_invalid" "${ExpectedOs}:$($entry.Name)" }
  }
  $subjectPath = Get-SafeFile $root "runtime-process.subject" "subject"
  $envelopePath = Get-SafeFile $root "runtime-process.envelope" "envelope"
  $capturePath = Get-SafeFile $root "suite.capture.json" "capture"
  $stdoutPath = Get-SafeFile $root "suite.stdout" "stdout"
  $stderrPath = Get-SafeFile $root "suite.stderr" "stderr"
  $subject = Parse-Manifest $subjectPath "subject"
  $base = @("format", "source_commit", "os", "arch", "executor", "run_id", "run_attempt", "job", "scenario_count")
  $expectedKeys = New-Object Collections.Generic.List[string]; $base | ForEach-Object { $expectedKeys.Add($_) }
  for ($i = 0; $i -lt 5; $i++) { $expectedKeys.Add("scenario.$i.id"); $expectedKeys.Add("scenario.$i.status"); $expectedKeys.Add("scenario.$i.fact_count"); for ($j = 0; $j -lt 64; $j++) { if ($subject.ContainsKey("scenario.$i.fact_count") -and $j -lt (Get-Int $subject["scenario.$i.fact_count"] "scenario.$i.fact_count" 1)) { $expectedKeys.Add("scenario.$i.fact.$j.name"); $expectedKeys.Add("scenario.$i.fact.$j.value") } } }
  Require-ExactKeys $subject $expectedKeys.ToArray() "subject"
  if ($subject.format -cne $SubjectFormat -or $subject.source_commit -cne $ExpectedSourceCommit -or $subject.os -cne $ExpectedOs) { Fail-Evidence "subject_identity_invalid" $ExpectedOs }
  if ($subject.arch -notmatch '^[a-z0-9_]+$' -or $subject.run_id -cne $ExpectedRunId -or $subject.run_attempt -cne $ExpectedRunAttempt -or $subject.job -notmatch '^[A-Za-z0-9_.-]+$') { Fail-Evidence "subject_identity_invalid" $ExpectedOs }
  if ($subject.executor -cne "github-actions:$ExpectedRunId`:$ExpectedRunAttempt`:$($subject.job):$ExpectedOs`:$($subject.arch)") { Fail-Evidence "executor_invalid" $ExpectedOs }
  if ((Get-Int $subject.scenario_count "scenario_count" 1) -ne 5) { Fail-Evidence "scenario_count_invalid" $ExpectedOs }
  $seen = @{}
  for ($i = 0; $i -lt 5; $i++) {
    $id = $subject["scenario.$i.id"]; if ($id -cne $RequiredScenarios[$i] -or $seen.ContainsKey($id)) { Fail-Evidence "scenario_invalid" "${ExpectedOs}:$id" }; $seen[$id] = $true
    if ($subject["scenario.$i.status"] -cne "passed") { Fail-Evidence "scenario_failed" "${ExpectedOs}:$id" }
    $facts = @{}; $count = Get-Int $subject["scenario.$i.fact_count"] "scenario.$i.fact_count" 1
    for ($j = 0; $j -lt $count; $j++) { $name = $subject["scenario.$i.fact.$j.name"]; if ($name -notmatch '^[a-z][a-z0-9_]*$' -or $facts.ContainsKey($name)) { Fail-Evidence "scenario_fact_invalid" "${id}:$name" }; $facts[$name] = $subject["scenario.$i.fact.$j.value"] }
    Assert-Facts $id $facts $ExpectedOs
  }
  foreach ($id in $RequiredScenarios) { if (-not $seen.ContainsKey($id)) { Fail-Evidence "scenario_missing" "${ExpectedOs}:$id" } }
  $envelope = Parse-Manifest $envelopePath "envelope"
  Require-ExactKeys $envelope @("format", "kind", "source", "source_commit", "environment", "executor", "timestamp", "subject_digest") "envelope"
  if ($envelope.format -cne $EnvelopeFormat -or $envelope.kind -cne "measurement" -or $envelope.source -cne "w1-runtime-process-suite" -or $envelope.source_commit -cne $ExpectedSourceCommit -or $envelope.executor -cne $subject.executor -or $envelope.environment -cne "os=$ExpectedOs;arch=$($subject.arch);run_id=$ExpectedRunId;run_attempt=$ExpectedRunAttempt;job=$($subject.job)" -or (Get-Int $envelope.timestamp "envelope.timestamp" 1) -lt 1) { Fail-Evidence "envelope_identity_invalid" $ExpectedOs }
  $actualSubjectDigest = Get-Sha256 ([IO.File]::ReadAllBytes($subjectPath)); if ($envelope.subject_digest -cne $actualSubjectDigest) { Fail-Evidence "envelope_subject_digest_invalid" $ExpectedOs }
  try { $capture = [IO.File]::ReadAllText($capturePath, $Utf8NoBom) | ConvertFrom-Json } catch { Fail-Evidence "capture_invalid" $ExpectedOs }
  $props = @($capture.PSObject.Properties.Name); $requiredCapture = @("format", "capture_id", "executable", "argv", "outcome", "started_at", "finished_at", "duration_ms", "exit_code", "failure_reason", "runner", "stdout", "stderr")
  foreach ($key in $requiredCapture) { if ($props -notcontains $key) { Fail-Evidence "capture_field_missing" "${ExpectedOs}:$key" } }; foreach ($key in $props) { if ($requiredCapture -notcontains $key) { Fail-Evidence "capture_field_unknown" "${ExpectedOs}:$key" } }
  if ($capture.format -cne $CaptureFormat -or $capture.outcome -cne "success" -or (Get-Int ([string]$capture.exit_code) "capture.exit_code" 0) -ne 0 -or [string]$capture.executable -cne "cargo") { Fail-Evidence "capture_outcome_invalid" $ExpectedOs }
  $expectedArgv = @("test", "--test", "background_runtime", "w1_real_process_evidence_covers_required_scenarios", "--", "--ignored", "--exact", "--test-threads=1")
  if (@($capture.argv).Count -ne $expectedArgv.Count) { Fail-Evidence "capture_argv_invalid" $ExpectedOs }
  for ($argvIndex = 0; $argvIndex -lt $expectedArgv.Count; $argvIndex++) { if ([string]$capture.argv[$argvIndex] -cne $expectedArgv[$argvIndex]) { Fail-Evidence "capture_argv_invalid" $ExpectedOs } }
  $runner = $capture.runner
  if ($null -eq $runner -or [string]$runner.provider -cne "github-actions" -or [string]$runner.run_id -cne $ExpectedRunId -or [string]$runner.run_attempt -cne $ExpectedRunAttempt -or [string]$runner.job -cne $subject.job -or ([string]$runner.os).ToLowerInvariant() -cne $ExpectedOs -or ([string]$runner.architecture).ToLowerInvariant() -cne $subject.arch) { Fail-Evidence "capture_runner_invalid" $ExpectedOs }
  foreach ($streamName in @("stdout", "stderr")) {
    $stream = $capture.$streamName; if ($null -eq $stream -or @($stream.PSObject.Properties.Name).Count -ne 3 -or @($stream.PSObject.Properties.Name) -notcontains "path" -or @($stream.PSObject.Properties.Name) -notcontains "byte_count" -or @($stream.PSObject.Properties.Name) -notcontains "sha256") { Fail-Evidence "capture_stream_invalid" "${ExpectedOs}:$streamName" }
    $path = if ($streamName -eq "stdout") { $stdoutPath } else { $stderrPath }; $bytes = [IO.File]::ReadAllBytes($path)
    $expectedStreamPath = if ($streamName -eq "stdout") { "suite.stdout" } else { "suite.stderr" }
    if ([string]$stream.path -cne $expectedStreamPath -or (Get-Int ([string]$stream.byte_count) "capture.$streamName.byte_count" 0) -ne $bytes.LongLength -or [string]$stream.sha256 -cne (Get-Sha256 $bytes)) { Fail-Evidence "capture_stream_digest_invalid" "${ExpectedOs}:$streamName" }
  }
  return [pscustomobject]@{ Os = $ExpectedOs; SubjectDigest = $actualSubjectDigest }
}

Assert-TrustedInputs
$root = Assert-Directory $EvidencePath "evidence path"
$children = @(Get-ChildItem -LiteralPath $root -Force)
$isSingle = [IO.File]::Exists((Join-Path $root "runtime-process.subject"))
if ($isSingle) {
  $subject = Parse-Manifest (Get-SafeFile $root "runtime-process.subject" "subject") "subject"
  if ($RequiredPlatforms -notcontains $subject.os) { Fail-Evidence "platform_invalid" $subject.os }
  Validate-Platform $root $subject.os
} else {
  $platformRoots = @{}
  foreach ($child in $children) {
    if (-not $child.PSIsContainer) { Fail-Evidence "platform_root_invalid" $child.Name }
    $subject = Parse-Manifest (Get-SafeFile $child.FullName "runtime-process.subject" "subject") "subject"
    $platform = [string]$subject.os
    if ($RequiredPlatforms -notcontains $platform -or $platformRoots.ContainsKey($platform)) { Fail-Evidence "platform_duplicate_or_unknown" $platform }
    $platformRoots[$platform] = $child.FullName
  }
  foreach ($platform in $RequiredPlatforms) { if (-not $platformRoots.ContainsKey($platform)) { Fail-Evidence "platform_missing" $platform }; Validate-Platform $platformRoots[$platform] $platform }
}
