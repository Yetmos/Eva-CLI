[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$EvidencePath,

  [Parameter(Mandatory = $true)]
  [ValidatePattern("^[0-9a-f]{40}$")]
  [string]$SourceCommit,

  [Parameter(Mandatory = $true)]
  [ValidatePattern("^[a-z0-9][a-z0-9-]{3,63}$")]
  [string]$RunId,

  [string]$RepositoryRoot = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$BundleSchema = "eva.windows.provider_multiprocess_local_evidence.v1"
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false, $true)

function Fail-LocalEvidence {
  param([string]$Reason, [string]$Detail)

  $safe = if ([string]::IsNullOrWhiteSpace($Detail)) { "none" } else { $Detail.Replace("`r", " ").Replace("`n", " ") }
  throw "[windows-provider-local-evidence] reason=$Reason detail=$safe"
}

function Get-FullPath {
  param([string]$Path)

  try {
    if ([System.IO.Path]::IsPathRooted($Path)) {
      return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $Path))
  } catch {
    Fail-LocalEvidence "path_invalid" $Path
  }
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
      Fail-LocalEvidence "path_reparse_point" ("{0}:{1}" -f $Field, $current)
    }
  }
}

function Get-Sha256File {
  param([string]$Path)

  $sha = [System.Security.Cryptography.SHA256]::Create()
  try {
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    return "sha256:$([System.BitConverter]::ToString($sha.ComputeHash($bytes)).Replace('-', '').ToLowerInvariant())"
  } finally {
    $sha.Dispose()
  }
}

function Write-NewUtf8Json {
  param([string]$Path, [object]$Value)

  $full = Get-FullPath $Path
  Assert-NoReparsePath -Path $full -Field "output"
  $stream = New-Object System.IO.FileStream($full, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
  try {
    $json = (($Value | ConvertTo-Json -Depth 16 -Compress).Replace("`r`n", "`n").Replace("`r", "`n"))
    $bytes = $Utf8NoBom.GetBytes("$json`n")
    $stream.Write($bytes, 0, $bytes.Length)
    $stream.Flush($true)
  } finally {
    $stream.Dispose()
  }
}

function Write-NewUtf8Text {
  param([string]$Path, [string]$Text)

  $full = Get-FullPath $Path
  Assert-NoReparsePath -Path $full -Field "output"
  $stream = New-Object System.IO.FileStream($full, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
  try {
    $bytes = $Utf8NoBom.GetBytes($Text)
    $stream.Write($bytes, 0, $bytes.Length)
    $stream.Flush($true)
  } finally {
    $stream.Dispose()
  }
}

if ($env:OS -ne "Windows_NT") {
  Fail-LocalEvidence "windows_required" ([System.Environment]::OSVersion.Platform.ToString())
}
if ([string]::IsNullOrWhiteSpace($RepositoryRoot)) {
  $RepositoryRoot = Split-Path -Parent $PSScriptRoot
}
$repositoryFull = Get-FullPath $RepositoryRoot
$evidenceFull = Get-FullPath $EvidencePath
Assert-NoReparsePath -Path $repositoryFull -Field "repository_root"
Assert-NoReparsePath -Path $evidenceFull -Field "evidence_root"
if (-not (Test-Path -LiteralPath $repositoryFull -PathType Container)) {
  Fail-LocalEvidence "repository_missing" $repositoryFull
}
if (Test-Path -LiteralPath $evidenceFull) {
  Fail-LocalEvidence "evidence_exists" $evidenceFull
}

$gitHeadOutput = @(& git -C $repositoryFull rev-parse HEAD)
$gitHead = if ($gitHeadOutput.Count -eq 1) { ([string]$gitHeadOutput[0]).Trim() } else { "" }
if ($LASTEXITCODE -ne 0 -or $gitHead -cne $SourceCommit) {
  Fail-LocalEvidence "source_commit_mismatch" $gitHead
}
$gitStatus = @(& git -C $repositoryFull status --porcelain=v1 --untracked-files=all)
if ($LASTEXITCODE -ne 0 -or $gitStatus.Count -ne 0) {
  Fail-LocalEvidence "repository_dirty" $repositoryFull
}

$captureScript = Join-Path $repositoryFull "scripts\capture-release-evidence.ps1"
$credentialScript = Join-Path $repositoryFull "scripts\test-credential-leak-scan.ps1"
foreach ($script in @($captureScript, $credentialScript)) {
  Assert-NoReparsePath -Path $script -Field "script"
  if (-not (Test-Path -LiteralPath $script -PathType Leaf)) {
    Fail-LocalEvidence "script_missing" $script
  }
}

$plans = @(
  [ordered]@{
    ordinal = 0
    id = "process-boundary"
    executable = "cargo"
    argv = @("test", "-p", "eva-adapter", "process_backend::tests::", "--", "--test-threads=1", "--nocapture")
    claims = @("pid_start_token", "windows_job", "descendant_cleanup", "pid_reuse_fence", "same_sid", "different_sid_reject", "unknown_sid_reject")
  },
  [ordered]@{
    ordinal = 1
    id = "restart-budget"
    executable = "cargo"
    argv = @("test", "-p", "eva-adapter", "runtime::tests::runtime_crash_loop_never_exceeds_durable_restart_budget", "--", "--exact", "--test-threads=1", "--nocapture")
    claims = @("bounded_restart_attempts", "budget_exhausted", "durable_restart_state")
  },
  [ordered]@{
    ordinal = 2
    id = "provider-admission"
    executable = "cargo"
    argv = @("test", "-p", "eva-storage", "provider_admission::tests::", "--", "--test-threads=1", "--nocapture")
    claims = @("capacity_one_winner", "crash_expiry_reclaim", "successor_fence")
  },
  [ordered]@{
    ordinal = 3
    id = "orphan-recovery"
    executable = "cargo"
    argv = @("test", "-p", "eva-runtime", "recovery::tests::", "--", "--test-threads=1", "--nocapture")
    claims = @("orphan_cleanup", "pid_reuse_reject", "legacy_identity_reject", "generation_restart_preserved")
  },
  [ordered]@{
    ordinal = 4
    id = "daemon-recovery"
    executable = "cargo"
    argv = @("test", "-p", "eva-runtime", "daemon::tests::daemon_start_recovers_interrupted_provider_process_state", "--", "--exact", "--test-threads=1", "--nocapture")
    claims = @("daemon_restart_cleanup", "interrupted_provider_retired")
  },
  [ordered]@{
    ordinal = 5
    id = "credential-secret-zero"
    executable = "powershell"
    argv = @("-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", $credentialScript, "-RepositoryRoot", $repositoryFull)
    claims = @("synthetic_canary_consumed", "stdout_stderr_redacted", "artifact_redacted", "scanner_negative_controls")
  }
)

[System.IO.Directory]::CreateDirectory((Join-Path $evidenceFull "captures")) | Out-Null
$scenarioRecords = New-Object System.Collections.Generic.List[object]
$runner = $null
$locationPushed = $false
try {
  Push-Location $repositoryFull
  $locationPushed = $true
  foreach ($plan in $plans) {
    $directoryName = "{0:D2}-{1}" -f [int]$plan.ordinal, [string]$plan.id
    $captureRoot = Join-Path (Join-Path $evidenceFull "captures") $directoryName
    [System.IO.Directory]::CreateDirectory($captureRoot) | Out-Null
    $capturePath = Join-Path $captureRoot "capture.json"
    & $captureScript `
      -Executable ([string]$plan.executable) `
      -ArgumentList ([string[]]$plan.argv) `
      -ManifestPath $capturePath `
      -StdoutPath (Join-Path $captureRoot "capture.stdout") `
      -StderrPath (Join-Path $captureRoot "capture.stderr") `
      -TimeoutMilliseconds 300000 `
      -CaptureId "w3-windows-local.$([string]$plan.id)"
    $capture = [System.IO.File]::ReadAllText($capturePath, $Utf8NoBom) | ConvertFrom-Json
    if ($null -eq $runner) {
      $runner = $capture.runner
    }
    $scenarioRecords.Add([ordered]@{
        ordinal = [int]$plan.ordinal
        id = [string]$plan.id
        capture_path = "captures/$directoryName/capture.json"
        claims = [string[]]$plan.claims
      })
  }
} finally {
  if ($locationPushed) {
    Pop-Location
  }
}

$finalGitHeadOutput = @(& git -C $repositoryFull rev-parse HEAD)
$finalGitHead = if ($finalGitHeadOutput.Count -eq 1) { ([string]$finalGitHeadOutput[0]).Trim() } else { "" }
$finalGitStatus = @(& git -C $repositoryFull status --porcelain=v1 --untracked-files=all)
if ($LASTEXITCODE -ne 0 -or $finalGitHead -cne $SourceCommit -or $finalGitStatus.Count -ne 0) {
  Fail-LocalEvidence "repository_changed_during_capture" $repositoryFull
}

$bundle = [ordered]@{
  schema = $BundleSchema
  status = "completed_repository_local"
  source_commit = $SourceCommit
  run_id = $RunId
  repository_root = $repositoryFull
  scope = [ordered]@{
    os = "Windows"
    production = $false
    service_identity_attested = $false
    real_vault_authority = $false
    credential_material = "synthetic_canary"
  }
  runner = $runner
  scenarios = [object[]]$scenarioRecords.ToArray()
  written_at = [System.DateTimeOffset]::UtcNow.ToString("o", [System.Globalization.CultureInfo]::InvariantCulture)
}
$bundlePath = Join-Path $evidenceFull "bundle.json"
$bundleDigestPath = Join-Path $evidenceFull "bundle.sha256"
Write-NewUtf8Json -Path $bundlePath -Value $bundle
Write-NewUtf8Text -Path $bundleDigestPath -Text "$(Get-Sha256File $bundlePath)`n"
Write-Output $bundlePath
