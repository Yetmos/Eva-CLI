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
  [string]$ExpectedRunnerProvider,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedRunnerIdentity,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedRunnerRunId,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedRunnerRunAttempt,

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ExpectedRunnerJob,

  [Parameter(Mandatory = $true)]
  [ValidatePattern("^sha256:[0-9a-f]{64}$")]
  [string]$ExpectedEvaExecutableSha256,

  [ValidateSet("Lifecycle", "Reboot")]
  [string]$Mode = "Lifecycle",

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
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false, $true)

function New-EvidenceError {
  param(
    [string]$Reason,
    [string]$Detail
  )

  $safeDetail = if ([string]::IsNullOrWhiteSpace($Detail)) { "none" } else { $Detail.Replace("`r", " ").Replace("`n", " ") }
  return "[platform-service-evidence] reason=$Reason detail=$safeDetail"
}

function Fail-Evidence {
  param(
    [string]$Reason,
    [string]$Detail
  )

  throw (New-EvidenceError -Reason $Reason -Detail $Detail)
}

function Get-PathComparison {
  if ($env:OS -eq "Windows_NT") {
    return [System.StringComparison]::OrdinalIgnoreCase
  }
  return [System.StringComparison]::Ordinal
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
    if ($_.Exception.Message.StartsWith("[platform-service-evidence]", [System.StringComparison]::Ordinal)) {
      throw
    }
    Fail-Evidence "path_invalid" $Path
  }
}

function Test-PathInside {
  param(
    [string]$Child,
    [string]$Parent
  )

  $comparison = Get-PathComparison
  $childFull = Get-FullPath $Child
  $parentFull = (Get-FullPath $Parent).TrimEnd([char[]]@('\', '/'))
  return $childFull.Equals($parentFull, $comparison) -or $childFull.StartsWith("$parentFull$([System.IO.Path]::DirectorySeparatorChar)", $comparison)
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
      Fail-Evidence "path_reparse_point" ("{0}:{1}" -f $Field, $current)
    }
  }
}

function Assert-NoReparseDescendants {
  param(
    [string]$Path,
    [string]$Field
  )

  Assert-NoReparsePath -Path $Path -Field $Field
  if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
    Fail-Evidence "evidence_root_missing" $Path
  }

  $pending = New-Object System.Collections.Generic.Queue[string]
  $pending.Enqueue((Get-FullPath $Path))
  while ($pending.Count -gt 0) {
    $current = $pending.Dequeue()
    foreach ($entry in @(Get-ChildItem -LiteralPath $current -Force -ErrorAction Stop)) {
      if (($entry.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        Fail-Evidence "path_reparse_point" ("{0}:{1}" -f $Field, $entry.FullName)
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
  if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
    Fail-Evidence "file_missing" $fullPath
  }
  return Get-Sha256Bytes ([System.IO.File]::ReadAllBytes($fullPath))
}

function Read-JsonFile {
  param(
    [string]$Path,
    [string]$Reason
  )

  $fullPath = Get-FullPath $Path
  Assert-NoReparsePath -Path $fullPath -Field "json_input"
  if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
    Fail-Evidence $Reason $fullPath
  }
  try {
    $text = [System.IO.File]::ReadAllText($fullPath, $Utf8NoBom)
    $convertFromJson = Get-Command ConvertFrom-Json -ErrorAction Stop
    if ($convertFromJson.Parameters.ContainsKey("DateKind")) {
      return $text | ConvertFrom-Json -DateKind String
    }
    return $text | ConvertFrom-Json
  } catch {
    Fail-Evidence "json_invalid" $fullPath
  }
}

function ConvertTo-CanonicalJson {
  param([object]$Value)

  return (($Value | ConvertTo-Json -Depth 16 -Compress).Replace("`r`n", "`n").Replace("`r", "`n"))
}

function Write-NewUtf8LfJson {
  param(
    [string]$Path,
    [object]$Value
  )

  $fullPath = Get-FullPath $Path
  Assert-NoReparsePath -Path $fullPath -Field "receipt_path"
  $parent = [System.IO.Path]::GetDirectoryName($fullPath)
  if (-not [string]::IsNullOrWhiteSpace($parent)) {
    Assert-NoReparsePath -Path $parent -Field "receipt_parent"
    [System.IO.Directory]::CreateDirectory($parent) | Out-Null
  }
  try {
    $stream = New-Object System.IO.FileStream($fullPath, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    try {
      $bytes = $Utf8NoBom.GetBytes("$(ConvertTo-CanonicalJson $Value)`n")
      $stream.Write($bytes, 0, $bytes.Length)
      $stream.Flush($true)
    } finally {
      $stream.Dispose()
    }
  } catch {
    Fail-Evidence "receipt_exists" $fullPath
  }
}

function Assert-Equal {
  param(
    [object]$Actual,
    [object]$Expected,
    [string]$Reason,
    [string]$Detail
  )

  if ([string]$Actual -cne [string]$Expected) {
    Fail-Evidence $Reason ("{0}: actual='{1}' expected='{2}'" -f $Detail, [string]$Actual, [string]$Expected)
  }
}

function Assert-True {
  param(
    [bool]$Condition,
    [string]$Reason,
    [string]$Detail
  )

  if (-not $Condition) {
    Fail-Evidence $Reason $Detail
  }
}

function Get-RequiredProperty {
  param(
    [object]$Object,
    [string]$Name,
    [string]$Reason
  )

  if ($null -eq $Object -or $null -eq $Object.PSObject.Properties[$Name]) {
    Fail-Evidence $Reason $Name
  }
  return $Object.PSObject.Properties[$Name].Value
}

function Assert-ExactPropertySet {
  param(
    [object]$Object,
    [string[]]$Names,
    [string]$Reason,
    [string]$Detail
  )

  if ($null -eq $Object) {
    Fail-Evidence $Reason $Detail
  }
  $actual = @($Object.PSObject.Properties | ForEach-Object { $_.Name } | Sort-Object)
  $expected = @($Names | Sort-Object)
  if (($actual -join "`n") -cne ($expected -join "`n")) {
    Fail-Evidence $Reason ("{0}: actual='{1}' expected='{2}'" -f $Detail, ($actual -join ","), ($expected -join ","))
  }
}

function Assert-JsonBoolean {
  param(
    [object]$Actual,
    [bool]$Expected,
    [string]$Reason,
    [string]$Detail
  )

  if (-not ($Actual -is [bool]) -or [bool]$Actual -ne $Expected) {
    Fail-Evidence $Reason ("{0}: actual='{1}' expected='{2}'" -f $Detail, [string]$Actual, [string]$Expected)
  }
}

function Assert-JsonInteger {
  param(
    [object]$Actual,
    [int64]$Expected,
    [string]$Reason,
    [string]$Detail
  )

  if (-not (($Actual -is [byte]) -or ($Actual -is [int16]) -or ($Actual -is [int]) -or ($Actual -is [long])) -or [int64]$Actual -ne $Expected) {
    Fail-Evidence $Reason ("{0}: actual='{1}' expected='{2}'" -f $Detail, [string]$Actual, [string]$Expected)
  }
}

function Assert-JsonIntegerMinimum {
  param(
    [object]$Actual,
    [int64]$Minimum,
    [string]$Reason,
    [string]$Detail
  )

  if (-not (($Actual -is [byte]) -or ($Actual -is [int16]) -or ($Actual -is [int]) -or ($Actual -is [long])) -or [int64]$Actual -lt $Minimum) {
    Fail-Evidence $Reason ("{0}: actual='{1}' minimum='{2}'" -f $Detail, [string]$Actual, [string]$Minimum)
  }
}

function Assert-JsonNull {
  param(
    [object]$Actual,
    [string]$Reason,
    [string]$Detail
  )

  if ($null -ne $Actual) {
    Fail-Evidence $Reason $Detail
  }
}

function Get-RelativeFromOriginalRoot {
  param(
    [string]$OriginalRoot,
    [string]$OriginalPath,
    [string]$Field
  )

  $comparison = Get-PathComparison
  $root = (Get-FullPath $OriginalRoot).TrimEnd([char[]]@('\', '/'))
  $path = Get-FullPath $OriginalPath
  if (-not ($path.Equals($root, $comparison) -or $path.StartsWith("$root$([System.IO.Path]::DirectorySeparatorChar)", $comparison))) {
    Fail-Evidence "path_escape" ("{0}:{1}" -f $Field, $OriginalPath)
  }
  $relative = $path.Substring($root.Length).TrimStart([char[]]@('\', '/'))
  return $relative.Replace('\', '/')
}

function Join-RelativePath {
  param(
    [string]$Root,
    [string]$Relative,
    [string]$Field
  )

  if ([string]::IsNullOrWhiteSpace($Relative) -or [System.IO.Path]::IsPathRooted($Relative) -or $Relative.Contains("..")) {
    Fail-Evidence "path_escape" ("{0}:{1}" -f $Field, $Relative)
  }
  $full = Get-FullPath (Join-Path (Get-FullPath $Root) $Relative)
  if (-not (Test-PathInside -Child $full -Parent $Root)) {
    Fail-Evidence "path_escape" ("{0}:{1}" -f $Field, $Relative)
  }
  return $full
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

function Get-PrepareRebootPlan {
  $lifecycle = @(Get-LifecyclePlan)
  return @($lifecycle[0..5])
}

function Get-ResumeRebootPlan {
  return @(
    @{ ordinal = 0; step_id = "status-resume-preflight"; command = "status"; state = "running"; mutation = $false },
    @{ ordinal = 1; step_id = "restart"; command = "restart"; state = "running"; mutation = $true },
    @{ ordinal = 2; step_id = "status-post-restart"; command = "status"; state = "running"; mutation = $false },
    @{ ordinal = 3; step_id = "stop"; command = "stop"; state = "stopped"; mutation = $true },
    @{ ordinal = 4; step_id = "stop-idempotent"; command = "stop"; state = "stopped"; mutation = $false },
    @{ ordinal = 5; step_id = "uninstall"; command = "uninstall"; state = "not_installed"; mutation = $true },
    @{ ordinal = 6; step_id = "uninstall-idempotent"; command = "uninstall"; state = "not_installed"; mutation = $false },
    @{ ordinal = 7; step_id = "status-final"; command = "status"; state = "not_installed"; mutation = $false }
  )
}

function Assert-RootLayout {
  param(
    [string]$Root,
    [string[]]$ExpectedRelativeFiles,
    [string[]]$ExpectedRelativeDirectories
  )

  $actualFiles = @(Get-ChildItem -LiteralPath $Root -Recurse -Force -File | ForEach-Object {
      $relative = $_.FullName.Substring((Get-FullPath $Root).Length).TrimStart([char[]]@('\', '/')).Replace('\', '/')
      $relative
    } | Sort-Object)
  $actualDirectories = @(Get-ChildItem -LiteralPath $Root -Recurse -Force -Directory | ForEach-Object {
      $relative = $_.FullName.Substring((Get-FullPath $Root).Length).TrimStart([char[]]@('\', '/')).Replace('\', '/')
      $relative
    } | Sort-Object)
  $expectedFilesSorted = @($ExpectedRelativeFiles | Sort-Object)
  $expectedDirectoriesSorted = @($ExpectedRelativeDirectories | Sort-Object)
  if (($actualFiles -join "`n") -cne ($expectedFilesSorted -join "`n")) {
    Fail-Evidence "file_set_mismatch" "files"
  }
  if (($actualDirectories -join "`n") -cne ($expectedDirectoriesSorted -join "`n")) {
    Fail-Evidence "file_set_mismatch" "directories"
  }
}

function Assert-DigestSidecar {
  param(
    [string]$JsonPath,
    [string]$DigestPath,
    [string]$Reason
  )

  $actual = Get-Sha256File $JsonPath
  if (-not (Test-Path -LiteralPath $DigestPath -PathType Leaf)) {
    Fail-Evidence $Reason $DigestPath
  }
  $digestText = [System.IO.File]::ReadAllText((Get-FullPath $DigestPath), $Utf8NoBom)
  if ($digestText -cnotmatch '^sha256:[0-9a-f]{64}\n$') {
    Fail-Evidence "digest_sidecar_invalid" $DigestPath
  }
  $expected = $digestText.Substring(0, $digestText.Length - 1)
  Assert-Equal $actual $expected "digest_mismatch" $JsonPath
  return $actual
}

function Assert-CaptureStream {
  param(
    [object]$Stream,
    [string]$CaptureDirectory,
    [string]$ExpectedName,
    [string]$Field
  )

  Assert-ExactPropertySet -Object $Stream -Names @("path", "byte_count", "sha256") -Reason "capture_stream_invalid" -Detail $Field
  $relativePath = [string](Get-RequiredProperty $Stream "path" "capture_stream_invalid")
  Assert-Equal $relativePath $ExpectedName "capture_stream_path_invalid" $Field
  $path = Join-RelativePath -Root $CaptureDirectory -Relative $relativePath -Field $Field
  if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
    Fail-Evidence "capture_stream_missing" $path
  }
  $bytes = [System.IO.File]::ReadAllBytes($path)
  Assert-JsonInteger (Get-RequiredProperty $Stream "byte_count" "capture_stream_invalid") ([int64]$bytes.Length) "capture_stream_size_mismatch" $Field
  Assert-Equal (Get-Sha256Bytes $bytes) ([string](Get-RequiredProperty $Stream "sha256" "capture_stream_invalid")) "capture_stream_digest_mismatch" $Field
  return [pscustomobject]@{
    Path = $path
    Text = [System.IO.File]::ReadAllText($path, $Utf8NoBom)
    Sha256 = Get-Sha256Bytes $bytes
    ByteCount = [int64]$bytes.Length
  }
}

function Assert-Runner {
  param([object]$Runner)

  Assert-Equal ([string](Get-RequiredProperty $Runner "provider" "capture_runner_invalid")) $ExpectedRunnerProvider "capture_runner_invalid" "provider"
  Assert-Equal ([string](Get-RequiredProperty $Runner "identity" "capture_runner_invalid")) $ExpectedRunnerIdentity "capture_runner_invalid" "identity"
  Assert-Equal ([string](Get-RequiredProperty $Runner "run_id" "capture_runner_invalid")) $ExpectedRunnerRunId "capture_runner_invalid" "run_id"
  Assert-Equal ([string](Get-RequiredProperty $Runner "run_attempt" "capture_runner_invalid")) $ExpectedRunnerRunAttempt "capture_runner_invalid" "run_attempt"
  Assert-Equal ([string](Get-RequiredProperty $Runner "job" "capture_runner_invalid")) $ExpectedRunnerJob "capture_runner_invalid" "job"
  Assert-Equal ([string](Get-RequiredProperty $Runner "os" "capture_runner_invalid")) "Windows" "capture_runner_invalid" "os"
}

function Assert-ServiceStdout {
  param(
    [string]$Text,
    [hashtable]$Plan,
    [string]$ServiceName
  )

  try {
    $response = $Text | ConvertFrom-Json
  } catch {
    Fail-Evidence "capture_stdout_json_invalid" $Plan.step_id
  }
  Assert-True ([bool](Get-RequiredProperty $response "ok" "capture_stdout_contract_invalid")) "capture_stdout_contract_invalid" "$($Plan.step_id):ok"
  Assert-Equal ([int](Get-RequiredProperty $response "exit_code" "capture_stdout_contract_invalid")) 0 "capture_stdout_contract_invalid" "$($Plan.step_id):exit_code"
  Assert-Equal ([string](Get-RequiredProperty $response "command" "capture_stdout_contract_invalid")) "service.$($Plan.command)" "capture_stdout_contract_invalid" "$($Plan.step_id):command"
  $data = Get-RequiredProperty $response "data" "capture_stdout_contract_invalid"
  Assert-Equal ([string](Get-RequiredProperty $data "kind" "capture_stdout_contract_invalid")) "windows_service" "capture_stdout_contract_invalid" "$($Plan.step_id):kind"
  Assert-Equal ([string](Get-RequiredProperty $data "service_name" "capture_stdout_contract_invalid")) $ServiceName "capture_stdout_contract_invalid" "$($Plan.step_id):service_name"
  Assert-True ([bool](Get-RequiredProperty $data "production_adapter" "capture_stdout_contract_invalid")) "capture_stdout_contract_invalid" "$($Plan.step_id):production_adapter"
  Assert-Equal ([string](Get-RequiredProperty $data "state" "capture_stdout_contract_invalid")) $Plan.state "capture_stdout_contract_invalid" "$($Plan.step_id):state"
  Assert-Equal ([bool](Get-RequiredProperty $data "mutation_executed" "capture_stdout_contract_invalid")) ([bool]$Plan.mutation) "capture_stdout_contract_invalid" "$($Plan.step_id):mutation"
}

function Assert-LifecycleEvidence {
  param(
    [string]$Root,
    [object]$Owner,
    [object]$Transcript,
    [string]$TranscriptDigest
  )

  $serviceName = "eva-ext01-$ExpectedRunId"
  Assert-Equal ([string]$Owner.format) $OwnerFormat "owner_invalid" "format"
  Assert-Equal ([string]$Owner.source_commit) $ExpectedSourceCommit "owner_invalid" "source_commit"
  Assert-Equal ([string]$Owner.run_id) $ExpectedRunId "owner_invalid" "run_id"
  Assert-Equal ([string]$Owner.service_name) $serviceName "owner_invalid" "service_name"
  Assert-Equal ([string]$Owner.eva_executable_sha256) $ExpectedEvaExecutableSha256 "owner_invalid" "eva_executable_sha256"

  Assert-Equal ([string]$Transcript.format) $HarnessFormat "transcript_invalid" "format"
  Assert-Equal ([string]$Transcript.mode) "Lifecycle" "transcript_mode_invalid" "mode"
  Assert-Equal ([string]$Transcript.status) "success" "transcript_status_invalid" "status"
  Assert-Equal ([string]$Transcript.source_commit) $ExpectedSourceCommit "transcript_subject_mismatch" "source_commit"
  Assert-Equal ([string]$Transcript.run_id) $ExpectedRunId "transcript_subject_mismatch" "run_id"
  Assert-Equal ([string]$Transcript.service_name) $serviceName "transcript_subject_mismatch" "service_name"
  Assert-Equal ([string]$Transcript.eva_executable_sha256) $ExpectedEvaExecutableSha256 "transcript_subject_mismatch" "eva_executable_sha256"
  Assert-Equal ([string]$Owner.project_root) ([string]$Transcript.project_root) "owner_invalid" "project_root"

  $authority = Get-RequiredProperty $Transcript "authority" "authority_invalid"
  foreach ($name in @("allowed", "is_windows", "is_admin", "execute", "controlled_host")) {
    Assert-True ([bool](Get-RequiredProperty $authority $name "authority_invalid")) "authority_invalid" $name
  }
  Assert-Equal (@(Get-RequiredProperty $authority "reasons" "authority_invalid").Count) 0 "authority_invalid" "reasons"
  Assert-Equal (@($Transcript.warnings).Count) 0 "transcript_warnings_invalid" "warnings"
  Assert-True ($null -eq $Transcript.continuation) "transcript_continuation_invalid" "continuation"

  $originalEvidenceRoot = [string]$Transcript.evidence_root
  Assert-True (-not [string]::IsNullOrWhiteSpace($originalEvidenceRoot)) "transcript_subject_mismatch" "evidence_root"
  $plans = @(Get-LifecyclePlan)
  $steps = @($Transcript.steps)
  Assert-Equal $steps.Count $plans.Count "step_count_invalid" "steps"

  $expectedFiles = New-Object System.Collections.Generic.List[string]
  $expectedDirectories = New-Object System.Collections.Generic.List[string]
  $expectedFiles.Add("platform-service-harness.owner.json")
  $expectedFiles.Add("transcript.lifecycle.json")
  $expectedFiles.Add("transcript.lifecycle.sha256")
  $expectedDirectories.Add("captures")

  for ($i = 0; $i -lt $plans.Count; $i += 1) {
    $plan = $plans[$i]
    $step = $steps[$i]
    $captureDirectoryRelative = "captures/{0:D2}-{1}" -f [int]$plan.ordinal, [string]$plan.step_id
    $expectedDirectories.Add($captureDirectoryRelative)
    $expectedFiles.Add("$captureDirectoryRelative/capture.json")
    $expectedFiles.Add("$captureDirectoryRelative/capture.stdout")
    $expectedFiles.Add("$captureDirectoryRelative/capture.stderr")

    Assert-Equal ([int]$step.ordinal) ([int]$plan.ordinal) "step_contract_invalid" "$($plan.step_id):ordinal"
    Assert-Equal ([string]$step.step_id) ([string]$plan.step_id) "step_contract_invalid" "$($plan.step_id):step_id"
    Assert-Equal ([string]$step.command) "service.$($plan.command)" "step_contract_invalid" "$($plan.step_id):command"
    Assert-Equal ([string]$step.expected_state) ([string]$plan.state) "step_contract_invalid" "$($plan.step_id):expected_state"
    Assert-Equal ([string]$step.actual_state) ([string]$plan.state) "step_contract_invalid" "$($plan.step_id):actual_state"
    Assert-Equal ([bool]$step.mutation_executed) ([bool]$plan.mutation) "step_contract_invalid" "$($plan.step_id):mutation"

    $captureRelative = Get-RelativeFromOriginalRoot -OriginalRoot $originalEvidenceRoot -OriginalPath ([string]$step.capture_path) -Field "$($plan.step_id):capture_path"
    Assert-Equal $captureRelative "$captureDirectoryRelative/capture.json" "step_capture_path_invalid" "$($plan.step_id):capture_path"
    $capturePath = Join-RelativePath -Root $Root -Relative $captureRelative -Field "$($plan.step_id):capture_path"
    $captureDirectory = [System.IO.Path]::GetDirectoryName($capturePath)
    $stdoutRelativeFromTranscript = Get-RelativeFromOriginalRoot -OriginalRoot $originalEvidenceRoot -OriginalPath ([string]$step.stdout_path) -Field "$($plan.step_id):stdout_path"
    Assert-Equal $stdoutRelativeFromTranscript "$captureDirectoryRelative/capture.stdout" "step_capture_path_invalid" "$($plan.step_id):stdout_path"

    $capture = Read-JsonFile -Path $capturePath -Reason "capture_missing"
    Assert-Equal ([string]$capture.format) $CaptureFormat "capture_invalid" "$($plan.step_id):format"
    Assert-Equal ([string]$capture.capture_id) "platform-service.$($plan.step_id)" "capture_invalid" "$($plan.step_id):capture_id"
    Assert-Equal ([string]$capture.outcome) "success" "capture_outcome_invalid" "$($plan.step_id):outcome"
    Assert-Equal ([int]$capture.exit_code) 0 "capture_outcome_invalid" "$($plan.step_id):exit_code"
    Assert-True ($null -eq $capture.failure_reason) "capture_outcome_invalid" "$($plan.step_id):failure_reason"
    $argv = @($capture.argv)
    Assert-Equal ($argv -join "`n") (@("service", [string]$plan.command, "--project", [string]$Transcript.project_root, "--output", "json") -join "`n") "capture_argv_invalid" "$($plan.step_id):argv"
    Assert-Runner (Get-RequiredProperty $capture "runner" "capture_runner_invalid")

    $stdout = Assert-CaptureStream -Stream (Get-RequiredProperty $capture "stdout" "capture_stream_invalid") -CaptureDirectory $captureDirectory -ExpectedName "capture.stdout" -Field "$($plan.step_id):stdout"
    $stderr = Assert-CaptureStream -Stream (Get-RequiredProperty $capture "stderr" "capture_stream_invalid") -CaptureDirectory $captureDirectory -ExpectedName "capture.stderr" -Field "$($plan.step_id):stderr"
    Assert-Equal $stderr.ByteCount 0 "capture_stderr_nonempty" "$($plan.step_id):stderr"
    Assert-Equal $stderr.Text "" "capture_stderr_nonempty" "$($plan.step_id):stderr"
    Assert-ServiceStdout -Text $stdout.Text -Plan $plan -ServiceName $serviceName
  }

  Assert-RootLayout -Root $Root -ExpectedRelativeFiles @($expectedFiles) -ExpectedRelativeDirectories @($expectedDirectories)
  return [pscustomobject]@{
    ServiceName = $serviceName
    TranscriptDigest = $TranscriptDigest
    CaptureCount = $plans.Count
  }
}

if ($Mode -eq "Reboot") {
  Fail-Evidence "mode_not_supported" "Reboot readback requires trusted reboot transcript artifacts."
}

$root = Get-FullPath $EvidencePath
$receiptFull = Get-FullPath $ReceiptPath
Assert-NoReparseDescendants -Path $root -Field "evidence_root"
if (Test-PathInside -Child $receiptFull -Parent $root) {
  Fail-Evidence "receipt_inside_evidence" $receiptFull
}
Assert-NoReparsePath -Path $receiptFull -Field "receipt_path"
if (Test-Path -LiteralPath $receiptFull) {
  Fail-Evidence "receipt_exists" $receiptFull
}

$ownerPath = Join-Path $root "platform-service-harness.owner.json"
$transcriptPath = Join-Path $root "transcript.lifecycle.json"
$transcriptDigestPath = Join-Path $root "transcript.lifecycle.sha256"
$owner = Read-JsonFile -Path $ownerPath -Reason "owner_missing"
$transcriptDigest = Assert-DigestSidecar -JsonPath $transcriptPath -DigestPath $transcriptDigestPath -Reason "transcript_digest_missing"
$transcript = Read-JsonFile -Path $transcriptPath -Reason "transcript_missing"
$result = Assert-LifecycleEvidence -Root $root -Owner $owner -Transcript $transcript -TranscriptDigest $transcriptDigest

$treeLines = @(Get-ChildItem -LiteralPath $root -Recurse -Force -File | ForEach-Object {
    $relative = $_.FullName.Substring($root.Length).TrimStart([char[]]@('\', '/')).Replace('\', '/')
    "$relative $((Get-Sha256File $_.FullName))"
  } | Sort-Object)
$treeDigest = Get-Sha256Bytes ($Utf8NoBom.GetBytes(($treeLines -join "`n") + "`n"))

$receipt = [ordered]@{
  format = $ReceiptFormat
  status = "verified_local_readback"
  mode = $Mode
  source_commit = $ExpectedSourceCommit
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
  transcript_digest = $result.TranscriptDigest
  capture_count = $result.CaptureCount
  tree_digest = $treeDigest
  verified_at = [System.DateTimeOffset]::UtcNow.ToString("o", [System.Globalization.CultureInfo]::InvariantCulture)
}

Write-NewUtf8LfJson -Path $receiptFull -Value $receipt
Write-Output $receiptFull
