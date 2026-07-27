[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Script:HarnessFormat = "eva.windows.platform_service_harness.v1"
$Script:ContinuationFormat = "eva.windows.platform_service_continuation.v1"
$Script:Utf8NoBom = New-Object System.Text.UTF8Encoding($false)

function New-HarnessError {
  param(
    [string]$Reason,
    [string]$Detail
  )

  $safeDetail = if ([string]::IsNullOrWhiteSpace($Detail)) {
    "none"
  } else {
    $Detail.Replace("`r", " ").Replace("`n", " ")
  }
  return "[platform-service-harness] reason=$Reason detail=$safeDetail"
}

function Fail-Harness {
  param(
    [string]$Reason,
    [string]$Detail
  )

  throw (New-HarnessError -Reason $Reason -Detail $Detail)
}

function Get-FullPath {
  param([string]$Path)

  try {
    if ([string]::IsNullOrWhiteSpace($Path)) {
      Fail-Harness "path_missing" "path"
    }
    if ([System.IO.Path]::IsPathRooted($Path)) {
      return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $Path))
  } catch {
    if ($_.Exception.Message.StartsWith("[platform-service-harness]", [System.StringComparison]::Ordinal)) {
      throw
    }
    Fail-Harness "path_invalid" $Path
  }
}

function Test-WindowsHost {
  return $env:OS -eq "Windows_NT"
}

function Get-PathComparison {
  if (Test-WindowsHost) {
    return [System.StringComparison]::OrdinalIgnoreCase
  }
  return [System.StringComparison]::Ordinal
}

function Assert-RunId {
  param([string]$RunId)

  if ([string]::IsNullOrWhiteSpace($RunId) -or $RunId -cnotmatch '^[a-z0-9][a-z0-9-]{3,63}$') {
    Fail-Harness "run_id_invalid" $RunId
  }
}

function Assert-SourceCommit {
  param([string]$SourceCommit)

  if ([string]::IsNullOrWhiteSpace($SourceCommit) -or $SourceCommit -cnotmatch '^[0-9a-f]{40}$') {
    Fail-Harness "source_commit_invalid" $SourceCommit
  }
}

function Resolve-SourceCommit {
  param(
    [string]$RepositoryRoot,
    [string]$SourceCommit
  )

  if (-not [string]::IsNullOrWhiteSpace($SourceCommit)) {
    Assert-SourceCommit $SourceCommit
    return $SourceCommit
  }

  $git = Get-Command git -ErrorAction SilentlyContinue
  if ($null -eq $git) {
    Fail-Harness "source_commit_required" "git is unavailable"
  }
  $resolvedOutput = @(& $git.Source -C (Get-FullPath $RepositoryRoot) rev-parse HEAD 2>$null)
  if ($LASTEXITCODE -ne 0) {
    Fail-Harness "source_commit_required" "git rev-parse failed"
  }
  $resolved = [string]$resolvedOutput[0]
  Assert-SourceCommit $resolved
  return $resolved
}

function Assert-RunScopedPath {
  param(
    [string]$Path,
    [string]$RunId,
    [string]$Field
  )

  $fullPath = Get-FullPath $Path
  if ($fullPath.IndexOf($RunId, [System.StringComparison]::OrdinalIgnoreCase) -lt 0) {
    Fail-Harness "run_scope_missing" ("{0}:{1}" -f $Field, $fullPath)
  }
}

function Assert-NoReparsePath {
  param(
    [string]$Path,
    [string]$Field
  )

  $fullPath = Get-FullPath $Path
  $root = [System.IO.Path]::GetPathRoot($fullPath)
  $current = $root
  $relative = $fullPath.Substring($root.Length)
  foreach ($part in $relative.Split([char[]]@('\', '/'), [System.StringSplitOptions]::RemoveEmptyEntries)) {
    $current = Join-Path $current $part
    if (-not (Test-Path -LiteralPath $current)) {
      break
    }
    $attributes = [System.IO.File]::GetAttributes($current)
    if (($attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
      Fail-Harness "path_reparse_point" ("{0}:{1}" -f $Field, $current)
    }
  }
}

function Assert-NoReparseDescendants {
  param(
    [string]$Path,
    [string]$Field
  )

  $fullPath = Get-FullPath $Path
  Assert-NoReparsePath -Path $fullPath -Field $Field
  if (-not (Test-Path -LiteralPath $fullPath -PathType Container)) {
    return
  }

  $pending = New-Object System.Collections.Generic.Queue[string]
  $pending.Enqueue($fullPath)
  while ($pending.Count -gt 0) {
    $current = $pending.Dequeue()
    foreach ($entry in @(Get-ChildItem -LiteralPath $current -Force -ErrorAction Stop)) {
      if (($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        Fail-Harness "path_reparse_point" ("{0}:{1}" -f $Field, $entry.FullName)
      }
      if ($entry.PSIsContainer) {
        $pending.Enqueue($entry.FullName)
      }
    }
  }
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

  $fullPath = Get-FullPath $Path
  Assert-NoReparsePath -Path $fullPath -Field "digest_input"
  return Get-Sha256Bytes ([System.IO.File]::ReadAllBytes($fullPath))
}

function ConvertTo-CanonicalJson {
  param([object]$Value)

  return (($Value | ConvertTo-Json -Depth 16 -Compress).Replace("`r`n", "`n").Replace("`r", "`n"))
}

function Write-Utf8LfJson {
  param(
    [string]$Path,
    [object]$Value
  )

  $fullPath = Get-FullPath $Path
  Assert-NoReparsePath -Path $fullPath -Field "json_output"
  $parent = [System.IO.Path]::GetDirectoryName($fullPath)
  if (-not [string]::IsNullOrWhiteSpace($parent)) {
    [System.IO.Directory]::CreateDirectory($parent) | Out-Null
  }
  $json = ConvertTo-CanonicalJson $Value
  [System.IO.File]::WriteAllText($fullPath, "$json`n", $Script:Utf8NoBom)
}

function Write-NewUtf8LfFile {
  param(
    [string]$Path,
    [string]$Text
  )

  $fullPath = Get-FullPath $Path
  Assert-NoReparsePath -Path $fullPath -Field "immutable_output"
  $parent = [System.IO.Path]::GetDirectoryName($fullPath)
  if (-not [string]::IsNullOrWhiteSpace($parent)) {
    [System.IO.Directory]::CreateDirectory($parent) | Out-Null
  }
  try {
    $stream = New-Object System.IO.FileStream(
      $fullPath,
      [System.IO.FileMode]::CreateNew,
      [System.IO.FileAccess]::Write,
      [System.IO.FileShare]::None
    )
    try {
      $bytes = $Script:Utf8NoBom.GetBytes($Text.Replace("`r`n", "`n").Replace("`r", "`n"))
      $stream.Write($bytes, 0, $bytes.Length)
      $stream.Flush($true)
    } finally {
      $stream.Dispose()
    }
  } catch {
    Fail-Harness "immutable_artifact_exists" $fullPath
  }
}

function Write-NewUtf8LfJson {
  param(
    [string]$Path,
    [object]$Value
  )

  Write-NewUtf8LfFile -Path $Path -Text "$(ConvertTo-CanonicalJson $Value)`n"
}

function Read-JsonFile {
  param([string]$Path)

  $fullPath = Get-FullPath $Path
  Assert-NoReparsePath -Path $fullPath -Field "json_input"
  $text = [System.IO.File]::ReadAllText($fullPath, $Script:Utf8NoBom)
  $convertFromJson = Get-Command ConvertFrom-Json -ErrorAction Stop
  if ($convertFromJson.Parameters.ContainsKey("DateKind")) {
    return $text | ConvertFrom-Json -DateKind String
  }
  return $text | ConvertFrom-Json
}

function Get-CurrentBootMarker {
  if (-not (Test-WindowsHost)) {
    Fail-Harness "windows_required" "boot marker"
  }

  try {
    $os = Get-CimInstance -ClassName Win32_OperatingSystem -ErrorAction Stop
    $lastBoot = $os.LastBootUpTime
    if ($lastBoot -is [System.DateTime]) {
      return $lastBoot.ToUniversalTime().ToString("o", [System.Globalization.CultureInfo]::InvariantCulture)
    }
    return ([System.Management.ManagementDateTimeConverter]::ToDateTime([string]$lastBoot)).ToUniversalTime().ToString("o", [System.Globalization.CultureInfo]::InvariantCulture)
  } catch {
    Fail-Harness "boot_marker_unavailable" $_.Exception.Message
  }
}

function Test-AdminToken {
  if (-not (Test-WindowsHost)) {
    return $false
  }

  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  $principal = New-Object Security.Principal.WindowsPrincipal($identity)
  return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-HarnessAuthority {
  param(
    [switch]$Execute,
    [switch]$ControlledHost,
    [bool]$IsWindows = (Test-WindowsHost),
    [bool]$IsAdmin = (Test-AdminToken)
  )

  $reasons = New-Object System.Collections.Generic.List[string]
  if (-not $Execute) {
    $reasons.Add("execute_required")
  }
  if (-not $ControlledHost) {
    $reasons.Add("controlled_host_required")
  }
  if (-not $IsWindows) {
    $reasons.Add("windows_required")
  }
  if (-not $IsAdmin) {
    $reasons.Add("elevated_admin_required")
  }

  return [pscustomobject]@{
    allowed = ($reasons.Count -eq 0)
    reasons = @($reasons)
    is_windows = $IsWindows
    is_admin = $IsAdmin
    execute = [bool]$Execute
    controlled_host = [bool]$ControlledHost
  }
}

function Assert-HarnessAuthority {
  param(
    [switch]$Execute,
    [switch]$ControlledHost
  )

  $authority = Get-HarnessAuthority -Execute:$Execute -ControlledHost:$ControlledHost
  if (-not $authority.allowed) {
    Fail-Harness "mutation_not_authorized" ($authority.reasons -join ",")
  }
  return $authority
}

function Get-EvaInvocation {
  param(
    [string]$RepositoryRoot,
    [string]$EvaExecutable
  )

  $repositoryFull = Get-FullPath $RepositoryRoot
  if (-not [string]::IsNullOrWhiteSpace($EvaExecutable)) {
    return [pscustomobject]@{
      executable = Get-FullPath $EvaExecutable
      prefix_args = @()
      descriptor = "eva-binary"
    }
  }

  $builtEva = Join-Path $repositoryFull "target\debug\eva.exe"
  if (Test-Path -LiteralPath $builtEva -PathType Leaf) {
    return [pscustomobject]@{
      executable = $builtEva
      prefix_args = @()
      descriptor = "eva-binary"
    }
  }

  $cargo = Get-Command cargo -ErrorAction SilentlyContinue
  if ($null -ne $cargo) {
    return [pscustomobject]@{
      executable = $cargo.Source
      prefix_args = @("run", "-q", "--")
      descriptor = "cargo-run"
    }
  }

  Fail-Harness "eva_invocation_missing" "target\\debug\\eva.exe or cargo"
}

function Get-StepDefinition {
  param(
    [int]$Ordinal,
    [string]$StepId,
    [string]$Command,
    [AllowNull()]
    [string]$ExpectedState,
    [AllowNull()]
    [Nullable[bool]]$MutationExecuted
  )

  return [ordered]@{
    ordinal = $Ordinal
    step_id = $StepId
    command = $Command
    expected_state = $ExpectedState
    expected_mutation_executed = $MutationExecuted
  }
}

function Get-LifecyclePlan {
  param([ValidateSet("Lifecycle", "PrepareReboot", "ResumeReboot")] [string]$Mode)

  $lifecycle = @(
    (Get-StepDefinition 0 "status-preflight" "status" "not_installed" $false),
    (Get-StepDefinition 1 "install" "install" "stopped" $true),
    (Get-StepDefinition 2 "install-idempotent" "install" "stopped" $false),
    (Get-StepDefinition 3 "start" "start" "running" $true),
    (Get-StepDefinition 4 "start-idempotent" "start" "running" $false),
    (Get-StepDefinition 5 "status-running" "status" "running" $false),
    (Get-StepDefinition 6 "restart" "restart" "running" $true),
    (Get-StepDefinition 7 "status-post-restart" "status" "running" $false),
    (Get-StepDefinition 8 "stop" "stop" "stopped" $true),
    (Get-StepDefinition 9 "stop-idempotent" "stop" "stopped" $false),
    (Get-StepDefinition 10 "uninstall" "uninstall" "not_installed" $true),
    (Get-StepDefinition 11 "uninstall-idempotent" "uninstall" "not_installed" $false),
    (Get-StepDefinition 12 "status-final" "status" "not_installed" $false)
  )

  switch ($Mode) {
    "Lifecycle" { return $lifecycle }
    "PrepareReboot" { return @($lifecycle[0..5]) }
    "ResumeReboot" { return @(
      (Get-StepDefinition 0 "status-resume-preflight" "status" "running" $false),
      (Get-StepDefinition 1 "restart" "restart" "running" $true),
      (Get-StepDefinition 2 "status-post-restart" "status" "running" $false),
      (Get-StepDefinition 3 "stop" "stop" "stopped" $true),
      (Get-StepDefinition 4 "stop-idempotent" "stop" "stopped" $false),
      (Get-StepDefinition 5 "uninstall" "uninstall" "not_installed" $true),
      (Get-StepDefinition 6 "uninstall-idempotent" "uninstall" "not_installed" $false),
      (Get-StepDefinition 7 "status-final" "status" "not_installed" $false)
    ) }
  }
}

function New-PlatformServiceContext {
  param(
    [string]$RepositoryRoot,
    [string]$RunId,
    [string]$SourceCommit,
    [string]$ProjectRoot,
    [string]$EvidenceRoot,
    [string]$EvaExecutable
  )

  Assert-RunId $RunId
  $repositoryFull = Get-FullPath $RepositoryRoot
  $resolvedSourceCommit = Resolve-SourceCommit -RepositoryRoot $repositoryFull -SourceCommit $SourceCommit
  $projectFull = Get-FullPath $ProjectRoot
  $evidenceFull = Get-FullPath $EvidenceRoot
  Assert-NoReparsePath -Path $repositoryFull -Field "repository_root"
  Assert-NoReparsePath -Path $projectFull -Field "project_root"
  Assert-NoReparsePath -Path $evidenceFull -Field "evidence_root"
  Assert-RunScopedPath -Path $projectFull -RunId $RunId -Field "project_root"
  Assert-RunScopedPath -Path $evidenceFull -RunId $RunId -Field "evidence_root"
  $serviceName = "eva-ext01-$RunId"
  $captureRoot = Join-Path $evidenceFull "captures"
  $continuationPath = Join-Path $evidenceFull "continuation.json"
  $continuationDigestPath = Join-Path $evidenceFull "continuation.sha256"
  $invocation = Get-EvaInvocation -RepositoryRoot $repositoryFull -EvaExecutable $EvaExecutable
  if (-not (Test-Path -LiteralPath $invocation.executable -PathType Leaf)) {
    Fail-Harness "eva_invocation_missing" $invocation.executable
  }
  Assert-NoReparsePath -Path $invocation.executable -Field "eva_executable"

  return [pscustomobject]@{
    format = $Script:HarnessFormat
    repository_root = $repositoryFull
    run_id = $RunId
    source_commit = $resolvedSourceCommit
    service_name = $serviceName
    project_root = $projectFull
    evidence_root = $evidenceFull
    capture_root = $captureRoot
    continuation_path = $continuationPath
    continuation_digest_path = $continuationDigestPath
    eva_invocation = $invocation
    eva_executable_sha256 = Get-Sha256File $invocation.executable
  }
}

function New-HarnessProjectLayout {
  param(
    [string]$RepositoryRoot,
    [string]$RunId,
    [string]$ProjectRoot,
    [switch]$StartOnBoot,
    [switch]$PreserveExisting
  )

  $repositoryFull = Get-FullPath $RepositoryRoot
  $projectFull = Get-FullPath $ProjectRoot
  Assert-RunScopedPath -Path $projectFull -RunId $RunId -Field "project_root"
  Assert-NoReparseDescendants -Path $projectFull -Field "project_root"

  $ownerRoot = Join-Path $projectFull ".eva"
  $ownerPath = Join-Path $ownerRoot "platform-service-harness.owner.json"
  if (Test-Path -LiteralPath $projectFull -PathType Container) {
    if (-not (Test-Path -LiteralPath $ownerPath -PathType Leaf)) {
      $entries = @(Get-ChildItem -LiteralPath $projectFull -Force)
      if ($entries.Count -gt 0) {
        Fail-Harness "project_not_harness_owned" $projectFull
      }
    } else {
      $owner = Read-JsonFile $ownerPath
      if ([string]$owner.format -cne "eva.windows.platform_service_project.v1" -or
          [string]$owner.run_id -cne $RunId -or
          -not ([string]$owner.repository_root).Equals($repositoryFull, (Get-PathComparison))) {
        Fail-Harness "project_owner_mismatch" $projectFull
      }
    }
  }

  [System.IO.Directory]::CreateDirectory($projectFull) | Out-Null
  Assert-NoReparsePath -Path $projectFull -Field "project_root"
  [System.IO.Directory]::CreateDirectory($ownerRoot) | Out-Null
  Assert-NoReparsePath -Path $ownerRoot -Field "project_owner_root"
  if (-not (Test-Path -LiteralPath $ownerPath -PathType Leaf)) {
    Write-NewUtf8LfJson -Path $ownerPath -Value ([ordered]@{
        format = "eva.windows.platform_service_project.v1"
        run_id = $RunId
        repository_root = $repositoryFull
      })
  }

  $targetConfigRoot = Join-Path $projectFull "config"
  Assert-NoReparsePath -Path $targetConfigRoot -Field "project_config_root"
  if ($PreserveExisting) {
    if (-not (Test-Path -LiteralPath (Join-Path $targetConfigRoot "eva.yaml") -PathType Leaf)) {
      Fail-Harness "project_config_missing" $targetConfigRoot
    }
    return
  }

  $sourceConfigRoot = Join-Path $repositoryFull "config"
  if (-not (Test-Path -LiteralPath $sourceConfigRoot -PathType Container)) {
    Fail-Harness "repository_config_missing" $sourceConfigRoot
  }
  [System.IO.Directory]::CreateDirectory($targetConfigRoot) | Out-Null
  Assert-NoReparsePath -Path $targetConfigRoot -Field "project_config_root"
  Get-ChildItem -LiteralPath $sourceConfigRoot -Force | ForEach-Object {
    Copy-Item -LiteralPath $_.FullName -Destination $targetConfigRoot -Recurse -Force
  }

  $startOnBootValue = if ($StartOnBoot) { "true" } else { "false" }

  $evaYaml = @"
runtime:
  env: ext01
  workspace: .
  data_dir: .eva/data
  script_dir: config/agents
  adapter_dir: config/adapters
  hot_reload: true

process:
  topology: supervisor_runtime_blue_green

service_manager:
  enabled: true
  kind: windows_service
  service_name: eva-ext01-$RunId
  start_on_boot: $startOnBootValue
  restart_supervisor: true

eventbus:
  backend: recoverable_in_process
  broadcast_capacity: 4096
  durable_log:
    path: .eva/data/eventlog
    durability: strict
    retention_days: 7
    replay_on_start: true
  dead_letter:
    enabled: true
    backend: sqlite
    retention_days: 7

upgrade:
  mode: blue_green
  warmup_timeout_ms: 30000
  drain_timeout_ms: 30000
  snapshot_required: true
  rollback_enabled: true
  ingress_policy: route_new_to_candidate

scheduler:
  target_overrides_topic: true
  default_delivery: fanout
  max_route_targets: 32

state:
  backend: sqlite
  sqlite_path: .eva/data/state.db

memory:
  storage:
    backend: sqlite
    sqlite_path: .eva/data/memory.db
    sqlite_bundled: true
    wal: true
  agent:
    enabled: true
    max_records_per_agent: 1000
  global:
    enabled: true
    write_mode: proposed

knowledge:
  storage:
    backend: sqlite
    sqlite_path: .eva/data/knowledge.db
    sqlite_bundled: true
    wal: true
  index:
    text:
      backend: sqlite_fts5
    vector:
      enabled: false
      backend: none

observability:
  log_level: info
  tracing: true
  metrics: true
  audit: true
  otel_endpoint_env: OTEL_EXPORTER_OTLP_ENDPOINT
  otel_exporter:
    endpoint: http://localhost:4318
    auth_header_env: OTEL_EXPORTER_OTLP_AUTH_HEADER
    batch_size: 32
    timeout_ms: 5000
    drop_policy: drop-new
    max_metric_labels: 8
  retention:
    sink: jsonl-file
    max_file_bytes: 8388608
    max_rotated_files: 16
    retain_for_ms: 604800000
    corrupt_record_policy: skip-and-report

config:
  agent_dir: config/agents
  adapter_dir: config/adapters
  capability_dir: config/capabilities
  policy_dir: config/policies
  route_file: config/routes/topics.yaml
  schema_dir: config/schemas
"@

  [System.IO.File]::WriteAllText((Join-Path $targetConfigRoot "eva.yaml"), $evaYaml.Replace("`r`n", "`n"), $Script:Utf8NoBom)
}

function Initialize-HarnessEvidenceLayout {
  param([pscustomobject]$Context)

  Assert-NoReparseDescendants -Path $Context.evidence_root -Field "evidence_root"
  $ownerPath = Join-Path $Context.evidence_root "platform-service-harness.owner.json"
  if (Test-Path -LiteralPath $Context.evidence_root -PathType Container) {
    if (-not (Test-Path -LiteralPath $ownerPath -PathType Leaf)) {
      $entries = @(Get-ChildItem -LiteralPath $Context.evidence_root -Force)
      if ($entries.Count -gt 0) {
        Fail-Harness "evidence_not_harness_owned" $Context.evidence_root
      }
    } else {
      $owner = Read-JsonFile $ownerPath
      if ([string]$owner.format -cne "eva.windows.platform_service_evidence.v1" -or
          [string]$owner.source_commit -cne [string]$Context.source_commit -or
          [string]$owner.run_id -cne [string]$Context.run_id -or
          [string]$owner.service_name -cne [string]$Context.service_name -or
          -not ([string]$owner.repository_root).Equals([string]$Context.repository_root, (Get-PathComparison)) -or
          -not ([string]$owner.project_root).Equals([string]$Context.project_root, (Get-PathComparison)) -or
          [string]$owner.eva_executable_sha256 -cne [string]$Context.eva_executable_sha256) {
        Fail-Harness "evidence_owner_mismatch" $Context.evidence_root
      }
    }
  }

  [System.IO.Directory]::CreateDirectory($Context.evidence_root) | Out-Null
  Assert-NoReparsePath -Path $Context.evidence_root -Field "evidence_root"
  if (-not (Test-Path -LiteralPath $ownerPath -PathType Leaf)) {
    Write-NewUtf8LfJson -Path $ownerPath -Value ([ordered]@{
        format = "eva.windows.platform_service_evidence.v1"
        source_commit = [string]$Context.source_commit
        run_id = [string]$Context.run_id
        service_name = [string]$Context.service_name
        repository_root = [string]$Context.repository_root
        project_root = [string]$Context.project_root
        eva_executable_sha256 = [string]$Context.eva_executable_sha256
      })
  }
  [System.IO.Directory]::CreateDirectory($Context.capture_root) | Out-Null
  Assert-NoReparsePath -Path $Context.capture_root -Field "capture_root"
}

function Invoke-CapturedServiceCommand {
  param(
    [pscustomobject]$Context,
    [hashtable]$Step,
    [string[]]$AllowedStates = @(),
    [switch]$AllowAnyMutationExecuted
  )

  $captureScript = Join-Path $Context.repository_root "scripts\capture-release-evidence.ps1"
  $captureDirectory = Join-Path $Context.capture_root ("{0:D2}-{1}" -f [int]$Step.ordinal, [string]$Step.step_id)
  $capturePath = Join-Path $captureDirectory "capture.json"
  Assert-NoReparseDescendants -Path $Context.capture_root -Field "capture_root"
  Assert-NoReparsePath -Path $captureDirectory -Field "capture_directory"
  if (Test-Path -LiteralPath $captureDirectory) {
    Fail-Harness "immutable_capture_exists" $captureDirectory
  }
  [System.IO.Directory]::CreateDirectory($captureDirectory) | Out-Null
  Assert-NoReparsePath -Path $captureDirectory -Field "capture_directory"

  $arguments = @($Context.eva_invocation.prefix_args + @(
      "service",
      [string]$Step.command,
      "--project",
      [string]$Context.project_root,
      "--output",
      "json"
    ))

  & $captureScript `
    -Executable $Context.eva_invocation.executable `
    -ArgumentList $arguments `
    -ManifestPath $capturePath `
    -TimeoutMilliseconds 300000 `
    -CaptureId ("platform-service.{0}" -f [string]$Step.step_id) `
    -NoFail | Out-Null

  $capture = Read-JsonFile $capturePath
  if ([string]$capture.outcome -cne "success" -or [int]$capture.exit_code -ne 0) {
    Fail-Harness "service_command_failed" ("{0}:{1}" -f [string]$Step.step_id, [string]$capture.failure_reason)
  }

  $stdoutPath = Join-Path $captureDirectory ([string]$capture.stdout.path)
  $stdoutText = [System.IO.File]::ReadAllText($stdoutPath, $Script:Utf8NoBom)
  $response = $stdoutText | ConvertFrom-Json
  $expectedCommand = "service.$([string]$Step.command)"
  if (-not [bool]$response.ok) {
    Fail-Harness "service_contract_not_ok" $expectedCommand
  }
  if ([string]$response.command -cne $expectedCommand) {
    Fail-Harness "service_contract_command" ("{0}:{1}" -f $expectedCommand, [string]$response.command)
  }

  $data = $response.data
  if ([string]$data.service_name -cne [string]$Context.service_name) {
    Fail-Harness "service_contract_service_name" ([string]$data.service_name)
  }
  if (-not [bool]$data.production_adapter) {
    Fail-Harness "service_contract_not_production" ([string]$Step.step_id)
  }
  if ([string]$data.kind -cne "windows_service") {
    Fail-Harness "service_contract_kind" ([string]$data.kind)
  }
  $acceptedStates = if (@($AllowedStates).Count -gt 0) {
    @($AllowedStates)
  } else {
    @([string]$Step.expected_state)
  }
  if (@($acceptedStates | Where-Object { $_ -ceq [string]$data.state }).Count -eq 0) {
    Fail-Harness "service_contract_state" ("{0}:{1}" -f ($acceptedStates -join "|"), [string]$data.state)
  }
  if (-not $AllowAnyMutationExecuted -and [bool]$data.mutation_executed -ne [bool]$Step.expected_mutation_executed) {
    Fail-Harness "service_contract_mutation" ("{0}:{1}" -f [bool]$Step.expected_mutation_executed, [bool]$data.mutation_executed)
  }

  return [ordered]@{
    ordinal = [int]$Step.ordinal
    step_id = [string]$Step.step_id
    command = [string]$response.command
    expected_state = [string]$Step.expected_state
    actual_state = [string]$data.state
    mutation_executed = [bool]$data.mutation_executed
    capture_path = $capturePath
    stdout_path = $stdoutPath
  }
}

function Invoke-CleanupToNotInstalled {
  param([pscustomobject]$Context)

  $statusStep = Get-StepDefinition 997 "cleanup-status" "status" "not_installed" $false
  $statusResult = Invoke-CapturedServiceCommand `
    -Context $Context `
    -Step $statusStep `
    -AllowedStates @("running", "stopped", "not_installed") `
    -AllowAnyMutationExecuted

  if ([string]$statusResult.actual_state -ceq "running") {
    Invoke-CapturedServiceCommand `
      -Context $Context `
      -Step (Get-StepDefinition 998 "cleanup-stop" "stop" "stopped" $true) `
      -AllowedStates @("stopped") `
      -AllowAnyMutationExecuted | Out-Null
  }

  if ([string]$statusResult.actual_state -cne "not_installed") {
    Invoke-CapturedServiceCommand `
      -Context $Context `
      -Step (Get-StepDefinition 999 "cleanup-uninstall" "uninstall" "not_installed" $true) `
      -AllowedStates @("not_installed") `
      -AllowAnyMutationExecuted | Out-Null
  }

  Invoke-CapturedServiceCommand `
    -Context $Context `
    -Step (Get-StepDefinition 1000 "cleanup-status-final" "status" "not_installed" $false) `
    -AllowedStates @("not_installed") `
    -AllowAnyMutationExecuted | Out-Null
}

function New-ContinuationManifest {
  param(
    [pscustomobject]$Context,
    [string]$ExpectedBootMarker
  )

  $preparedBootMarker = Get-CurrentBootMarker
  return [ordered]@{
    format = $Script:ContinuationFormat
    source_commit = [string]$Context.source_commit
    run_id = [string]$Context.run_id
    service_name = [string]$Context.service_name
    project_root = [string]$Context.project_root
    evidence_root = [string]$Context.evidence_root
    eva_executable = [string]$Context.eva_invocation.executable
    eva_executable_sha256 = [string]$Context.eva_executable_sha256
    prepared_boot_marker = $preparedBootMarker
    expected_boot_marker = $ExpectedBootMarker
    prepared_at = [System.DateTimeOffset]::UtcNow.ToString("o", [System.Globalization.CultureInfo]::InvariantCulture)
  }
}

function Write-ContinuationArtifacts {
  param(
    [pscustomobject]$Context,
    [string]$ExpectedBootMarker
  )

  Assert-NoReparsePath -Path $Context.continuation_path -Field "continuation_path"
  Assert-NoReparsePath -Path $Context.continuation_digest_path -Field "continuation_digest_path"
  $manifest = New-ContinuationManifest -Context $Context -ExpectedBootMarker $ExpectedBootMarker
  Write-NewUtf8LfJson -Path $Context.continuation_path -Value $manifest
  $digest = Get-Sha256File $Context.continuation_path
  Write-NewUtf8LfFile -Path $Context.continuation_digest_path -Text "$digest`n"
  return [pscustomobject]@{
    manifest = $manifest
    digest = $digest
    path = $Context.continuation_path
    digest_path = $Context.continuation_digest_path
  }
}

function Read-ContinuationArtifacts {
  param(
    [string]$ContinuationPath,
    [string]$ExpectedDigest
  )

  $fullPath = Get-FullPath $ContinuationPath
  $actualDigest = Get-Sha256File $fullPath
  if ($actualDigest -cne $ExpectedDigest) {
    Fail-Harness "continuation_digest_mismatch" ("expected=$ExpectedDigest actual=$actualDigest")
  }
  return Read-JsonFile $fullPath
}

function Assert-ResumeBinding {
  param(
    [pscustomobject]$Context,
    [object]$Continuation,
    [string]$ExpectedDigest,
    [string]$CurrentBootMarker
  )

  if ([string]$Continuation.format -cne $Script:ContinuationFormat) {
    Fail-Harness "continuation_format_invalid" ([string]$Continuation.format)
  }
  if ([string]$Continuation.source_commit -cne [string]$Context.source_commit) {
    Fail-Harness "continuation_source_commit_mismatch" ([string]$Continuation.source_commit)
  }
  if ([string]$Continuation.run_id -cne [string]$Context.run_id) {
    Fail-Harness "continuation_run_id_mismatch" ([string]$Continuation.run_id)
  }
  if ([string]$Continuation.service_name -cne [string]$Context.service_name) {
    Fail-Harness "continuation_service_name_mismatch" ([string]$Continuation.service_name)
  }
  if (-not (Get-FullPath ([string]$Continuation.project_root)).Equals((Get-FullPath ([string]$Context.project_root)), (Get-PathComparison))) {
    Fail-Harness "continuation_project_root_mismatch" ([string]$Continuation.project_root)
  }
  if (-not (Get-FullPath ([string]$Continuation.evidence_root)).Equals((Get-FullPath ([string]$Context.evidence_root)), (Get-PathComparison))) {
    Fail-Harness "continuation_evidence_root_mismatch" ([string]$Continuation.evidence_root)
  }
  if (-not (Get-FullPath ([string]$Continuation.eva_executable)).Equals((Get-FullPath ([string]$Context.eva_invocation.executable)), (Get-PathComparison)) -or
      [string]$Continuation.eva_executable_sha256 -cne [string]$Context.eva_executable_sha256) {
    Fail-Harness "continuation_executable_mismatch" ([string]$Continuation.eva_executable)
  }
  if ([string]$CurrentBootMarker -ceq [string]$Continuation.prepared_boot_marker) {
    Fail-Harness "continuation_boot_marker_unchanged" [string]$CurrentBootMarker
  }
  if (-not [string]::IsNullOrWhiteSpace([string]$Continuation.expected_boot_marker) -and
      [string]$CurrentBootMarker -cne [string]$Continuation.expected_boot_marker) {
    Fail-Harness "continuation_boot_marker_mismatch" ("expected=$([string]$Continuation.expected_boot_marker) actual=$CurrentBootMarker")
  }

  return [pscustomobject]@{
    digest = $ExpectedDigest
    current_boot_marker = $CurrentBootMarker
  }
}

function Invoke-HarnessCleanup {
  param(
    [pscustomobject]$Context,
    [scriptblock[]]$CleanupActions
  )

  $errors = New-Object System.Collections.Generic.List[string]
  foreach ($action in @($CleanupActions)) {
    if ($null -eq $action) {
      continue
    }
    try {
      & $action
    } catch {
      $errors.Add($_.Exception.Message)
    }
  }
  return @($errors)
}

function Complete-HarnessResult {
  param(
    [bool]$OperationSucceeded,
    [string[]]$CleanupErrors,
    [string]$OperationName
  )

  $errors = @($CleanupErrors)
  if ($errors.Count -gt 0) {
    Fail-Harness "cleanup_failed" ("{0}:{1}" -f $OperationName, ($errors -join " | "))
  }
  return [pscustomobject]@{
    succeeded = $OperationSucceeded
    operation = $OperationName
    cleanup_errors = @()
  }
}

function Write-TranscriptArtifacts {
  param(
    [pscustomobject]$Context,
    [string]$Mode,
    [string]$Status,
    [object[]]$Steps,
    [object]$Authority,
    [object]$Continuation,
    [string[]]$Warnings
  )

  $transcript = [ordered]@{
    format = $Script:HarnessFormat
    mode = $Mode
    status = $Status
    source_commit = [string]$Context.source_commit
    run_id = [string]$Context.run_id
    service_name = [string]$Context.service_name
    project_root = [string]$Context.project_root
    evidence_root = [string]$Context.evidence_root
    eva_executable = [string]$Context.eva_invocation.executable
    eva_executable_sha256 = [string]$Context.eva_executable_sha256
    authority = $Authority
    steps = @($Steps)
    continuation = $Continuation
    warnings = @($Warnings)
    written_at = [System.DateTimeOffset]::UtcNow.ToString("o", [System.Globalization.CultureInfo]::InvariantCulture)
  }

  $transcriptStem = "transcript.$($Mode.ToLowerInvariant())"
  $transcriptPath = Join-Path $Context.evidence_root "$transcriptStem.json"
  $transcriptDigestPath = Join-Path $Context.evidence_root "$transcriptStem.sha256"
  Assert-NoReparsePath -Path $transcriptPath -Field "transcript_path"
  Assert-NoReparsePath -Path $transcriptDigestPath -Field "transcript_digest_path"
  Write-NewUtf8LfJson -Path $transcriptPath -Value $transcript
  $digest = Get-Sha256File $transcriptPath
  Write-NewUtf8LfFile -Path $transcriptDigestPath -Text "$digest`n"
  return [pscustomobject]@{
    transcript = $transcript
    digest = $digest
    path = $transcriptPath
    digest_path = $transcriptDigestPath
  }
}

function Invoke-PlatformServiceMode {
  param(
    [ValidateSet("Validate", "Lifecycle", "PrepareReboot", "ResumeReboot")] [string]$Mode,
    [string]$RepositoryRoot,
    [string]$RunId,
    [string]$SourceCommit,
    [string]$ProjectRoot,
    [string]$EvidenceRoot,
    [string]$EvaExecutable,
    [switch]$Execute,
    [switch]$ControlledHost,
    [switch]$AllowExternalContinuation,
    [string]$ExpectedBootMarker,
    [string]$ContinuationPath,
    [string]$ExpectedContinuationDigest,
    [string]$CurrentBootMarker
  )

  $context = New-PlatformServiceContext -RepositoryRoot $RepositoryRoot -RunId $RunId -SourceCommit $SourceCommit -ProjectRoot $ProjectRoot -EvidenceRoot $EvidenceRoot -EvaExecutable $EvaExecutable

  $authority = Get-HarnessAuthority -Execute:$Execute -ControlledHost:$ControlledHost
  $plan = Get-LifecyclePlan -Mode $(if ($Mode -eq "Validate") { "Lifecycle" } else { $Mode })

  if ($Mode -eq "Validate") {
    New-HarnessProjectLayout -RepositoryRoot $context.repository_root -RunId $RunId -ProjectRoot $context.project_root
    Initialize-HarnessEvidenceLayout -Context $context
    return (Write-TranscriptArtifacts -Context $context -Mode $Mode -Status "plan_only" -Steps $plan -Authority $authority -Continuation $null -Warnings @("non_mutating_validation"))
  }

  Assert-HarnessAuthority -Execute:$Execute -ControlledHost:$ControlledHost | Out-Null
  if ([string]$context.eva_invocation.descriptor -cne "eva-binary") {
    Fail-Harness "eva_binary_required" "build target/debug/eva.exe or pass -EvaExecutable"
  }
  if ($Mode -eq "PrepareReboot" -and -not $AllowExternalContinuation) {
    Fail-Harness "continuation_switch_required" "AllowExternalContinuation"
  }

  $continuation = $null
  if ($Mode -eq "ResumeReboot") {
    $resolvedContinuationPath = if ([string]::IsNullOrWhiteSpace($ContinuationPath)) { $context.continuation_path } else { $ContinuationPath }
    $resolvedBootMarker = if ([string]::IsNullOrWhiteSpace($CurrentBootMarker)) { Get-CurrentBootMarker } else { $CurrentBootMarker }
    $continuationManifest = Read-ContinuationArtifacts -ContinuationPath $resolvedContinuationPath -ExpectedDigest $ExpectedContinuationDigest
    $continuation = Assert-ResumeBinding -Context $context -Continuation $continuationManifest -ExpectedDigest $ExpectedContinuationDigest -CurrentBootMarker $resolvedBootMarker
  }

  Initialize-HarnessEvidenceLayout -Context $context
  if ($Mode -eq "ResumeReboot") {
    New-HarnessProjectLayout -RepositoryRoot $context.repository_root -RunId $RunId -ProjectRoot $context.project_root -PreserveExisting
  } else {
    New-HarnessProjectLayout -RepositoryRoot $context.repository_root -RunId $RunId -ProjectRoot $context.project_root -StartOnBoot:($Mode -eq "PrepareReboot")
  }

  $stepResults = New-Object System.Collections.Generic.List[object]
  $cleanupActions = New-Object System.Collections.Generic.List[scriptblock]
  $cleanupArmed = ($Mode -eq "ResumeReboot")
  $status = "success"
  try {
    foreach ($step in $plan) {
      if ([string]$step.step_id -ceq "install") {
        $cleanupArmed = $true
      }
      $stepResults.Add((Invoke-CapturedServiceCommand -Context $context -Step $step))
    }

    if ($Mode -eq "PrepareReboot") {
      $continuation = Write-ContinuationArtifacts -Context $context -ExpectedBootMarker $ExpectedBootMarker
      return (Write-TranscriptArtifacts -Context $context -Mode $Mode -Status "continuation_ready" -Steps @($stepResults) -Authority $authority -Continuation $continuation -Warnings @())
    }

    return (Write-TranscriptArtifacts -Context $context -Mode $Mode -Status $status -Steps @($stepResults) -Authority $authority -Continuation $continuation -Warnings @())
  } catch {
    $status = "failed"
    if ($cleanupArmed) {
      $cleanupActions.Add({ Invoke-CleanupToNotInstalled -Context $context })
    }
    $cleanupErrors = Invoke-HarnessCleanup -Context $context -CleanupActions @($cleanupActions)
    $null = Write-TranscriptArtifacts -Context $context -Mode $Mode -Status $status -Steps @($stepResults) -Authority $authority -Continuation $continuation -Warnings @($_.Exception.Message)
    if ($cleanupErrors.Count -gt 0) {
      Fail-Harness "cleanup_failed" (($cleanupErrors + @($_.Exception.Message)) -join " | ")
    }
    throw
  }
}
