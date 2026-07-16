[CmdletBinding()]
param()
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$Utf8 = New-Object System.Text.UTF8Encoding($false)
$Validator = Join-Path $PSScriptRoot "validate-runtime-process-evidence.ps1"
$Commit = "0123456789abcdef0123456789abcdef01234567"
$RunId = "12345"; $Attempt = "2"
$Root = Join-Path ([IO.Path]::GetTempPath()) ("eva-runtime-evidence-" + [Guid]::NewGuid().ToString("N"))
function Hash([byte[]]$Bytes) { $sha = [Security.Cryptography.SHA256]::Create(); try { "sha256:$([BitConverter]::ToString($sha.ComputeHash($Bytes)).Replace('-', '').ToLowerInvariant())" } finally { $sha.Dispose() } }
function WriteText([string]$Path, [string]$Text) { [IO.File]::WriteAllText($Path, $Text.Replace("`r`n", "`n"), $Utf8) }
function Invoke-Validator([string]$Path) { & $Validator -EvidencePath $Path -ExpectedSourceCommit $Commit -ExpectedRunId $RunId -ExpectedRunAttempt $Attempt | Out-Null }
function Assert-Fails([scriptblock]$Action, [string]$Name) { try { & $Action } catch { return }; throw "[runtime-process-evidence-test] expected failure: $Name" }
function New-Fixture([string]$Platform) {
  $dir = Join-Path $Root $Platform; [IO.Directory]::CreateDirectory($dir) | Out-Null
  $facts = @(
    @("background_owner", @(@("child_pid_positive", "true"), @("controlled_shutdown", "true"), @("released", "true"))), @("forced_kill", @(@("termination", "forced"), @("mechanism", $(if ($Platform -eq "windows") { "terminate_process" } else { "sigkill" })), @("owner_dead", "true"), @("stale_pid_preserved", "true"))), @("stale_lock_reclaim", @(@("immediate_restart_blocked", "true"), @("new_generation_gt_old", "true"), @("old_generation", "1"), @("new_generation", "2"))), @("restart_recovery", @(@("status", "completed"), @("attempts_min", "2"), @("generation_increased", "true"), @("result_digest_present", "true"))), @("effect_dedup", @(@("applied_count", "1"), @("committed_applied_count", "1"), @("prepared_applied_count", "1"), @("duplicate", "false"), @("status", "interrupted"), @("operator_block", "true"), @("committed_reused", "true"), @("committed_status", "completed"), @("prepared_attempts", "1"), @("stable_restart", "true"))) )
  $lines = @("format=eva.runtime-process.subject.v1", "source_commit=$Commit", "os=$Platform", "arch=x64", "executor=github-actions:$RunId`:$Attempt`:runtime-$Platform`:$Platform`:x64", "run_id=$RunId", "run_attempt=$Attempt", "job=runtime-$Platform", "scenario_count=5")
  for ($i = 0; $i -lt 5; $i++) { $entry = $facts[$i]; $lines += "scenario.$i.id=$($entry[0])"; $lines += "scenario.$i.status=passed"; $lines += "scenario.$i.fact_count=$($entry[1].Count)"; for ($j = 0; $j -lt $entry[1].Count; $j++) { $lines += "scenario.$i.fact.$j.name=$($entry[1][$j][0])"; $lines += "scenario.$i.fact.$j.value=$($entry[1][$j][1])" } }
  $subject = Join-Path $dir "runtime-process.subject"; WriteText $subject (($lines -join "`n") + "`n")
  $digest = Hash ([IO.File]::ReadAllBytes($subject)); $env = @("format=eva.release.evidence_envelope.v1", "kind=measurement", "source=w1-runtime-process-suite", "source_commit=$Commit", "environment=os=$Platform;arch=x64;run_id=$RunId;run_attempt=$Attempt;job=runtime-$Platform", "executor=github-actions:$RunId`:$Attempt`:runtime-$Platform`:$Platform`:x64", "timestamp=1", "subject_digest=$digest") -join "`n"; WriteText (Join-Path $dir "runtime-process.envelope") "$env`n"
  [byte[]]$out = [Text.Encoding]::UTF8.GetBytes("ok`n"); [byte[]]$err = @(); [IO.File]::WriteAllBytes((Join-Path $dir "suite.stdout"), $out); [IO.File]::WriteAllBytes((Join-Path $dir "suite.stderr"), $err)
  $capture = [ordered]@{ format="eva.release.command_capture.v1"; capture_id="runtime-process"; executable="cargo"; argv=@("test", "--test", "background_runtime", "w1_real_process_evidence_covers_required_scenarios", "--", "--ignored", "--exact", "--test-threads=1"); outcome="success"; started_at="2026-01-01T00:00:00.0000000+00:00"; finished_at="2026-01-01T00:00:01.0000000+00:00"; duration_ms=1; exit_code=0; failure_reason=$null; runner=[ordered]@{ provider="github-actions"; run_id=$RunId; run_attempt=$Attempt; job="runtime-$Platform"; os=$Platform; architecture="x64"; identity="contract"; name="contract" }; stdout=[ordered]@{ path="suite.stdout"; byte_count=$out.Length; sha256=(Hash $out) }; stderr=[ordered]@{ path="suite.stderr"; byte_count=0; sha256=(Hash $err) } } | ConvertTo-Json -Depth 5 -Compress
  WriteText (Join-Path $dir "suite.capture.json") "$capture`n"
  return $dir
}
try {
  foreach ($p in @("windows", "linux", "macos")) { New-Fixture $p | Out-Null }; Invoke-Validator $Root; Invoke-Validator (Join-Path $Root "windows")
  $subject = Join-Path $Root "linux/runtime-process.subject"; WriteText $subject (([IO.File]::ReadAllText($subject, $Utf8)) + "junk=true`n"); Assert-Fails { Invoke-Validator $Root } "extra field"; New-Fixture "linux" | Out-Null
  $subject = Join-Path $Root "macos/runtime-process.subject"; $missing = [IO.File]::ReadAllText($subject, $Utf8).Replace("scenario.4.id=effect_dedup`n", ""); WriteText $subject $missing; Assert-Fails { Invoke-Validator $Root } "missing scenario"; New-Fixture "macos" | Out-Null
  $subject = Join-Path $Root "windows/runtime-process.subject"; $failed = [IO.File]::ReadAllText($subject, $Utf8).Replace("scenario.0.status=passed", "scenario.0.status=failed"); WriteText $subject $failed; Assert-Fails { Invoke-Validator $Root } "failed scenario"; New-Fixture "windows" | Out-Null
  [IO.File]::AppendAllText((Join-Path $Root "linux/suite.stdout"), "tamper", $Utf8); Assert-Fails { Invoke-Validator $Root } "capture stream digest"; New-Fixture "linux" | Out-Null
  Copy-Item -LiteralPath (Join-Path $Root "windows") -Destination (Join-Path $Root "duplicate-windows") -Recurse; Assert-Fails { Invoke-Validator $Root } "duplicate platform"; Remove-Item -LiteralPath (Join-Path $Root "duplicate-windows") -Recurse -Force
  WriteText (Join-Path $Root "macos/unindexed.txt") "unexpected`n"; Assert-Fails { Invoke-Validator $Root } "extra platform file"; Remove-Item -LiteralPath (Join-Path $Root "macos/unindexed.txt") -Force
  $subject = Join-Path $Root "macos/runtime-process.subject"; $wrong = [IO.File]::ReadAllText($subject, $Utf8).Replace("source_commit=$Commit", "source_commit=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"); WriteText $subject $wrong; Assert-Fails { Invoke-Validator $Root } "mixed commit"
  Write-Output "runtime process evidence contract tests passed"
} finally { if ([IO.Directory]::Exists($Root)) { Remove-Item -LiteralPath $Root -Recurse -Force } }
