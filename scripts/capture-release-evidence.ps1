[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$Executable,

  [AllowEmptyCollection()]
  [string[]]$ArgumentList = @(),

  [Parameter(Mandatory = $true)]
  [ValidateNotNullOrEmpty()]
  [string]$ManifestPath,

  [string]$StdoutPath,
  [string]$StderrPath,

  [ValidateRange(1, 2147483647)]
  [int]$TimeoutMilliseconds = 300000,

  [ValidatePattern("^[a-z0-9][a-z0-9._-]{0,127}$")]
  [string]$CaptureId = "command",

  [switch]$NoFail
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$CaptureFormat = "eva.release.command_capture.v1"
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)

function Resolve-OutputPath {
  param([string]$Path)

  if ([System.IO.Path]::IsPathRooted($Path)) {
    return [System.IO.Path]::GetFullPath($Path)
  }
  return [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $Path))
}

function Assert-ManifestChildPath {
  param(
    [string]$ManifestDirectory,
    [string]$Path,
    [string]$Field
  )

  $comparison = if ($env:OS -eq "Windows_NT") {
    [System.StringComparison]::OrdinalIgnoreCase
  } else {
    [System.StringComparison]::Ordinal
  }
  $pathDirectory = [System.IO.Path]::GetDirectoryName($Path)
  if (-not $pathDirectory.Equals($ManifestDirectory, $comparison)) {
    throw "$Field must be stored directly in the manifest directory."
  }
  if (Test-Path -LiteralPath $Path) {
    $attributes = [System.IO.File]::GetAttributes($Path)
    if (($attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
      throw "$Field must not reference a symbolic link or reparse point."
    }
  }
}

function ConvertTo-ManifestRelativePath {
  param(
    [string]$ManifestDirectory,
    [string]$Path
  )

  $separator = [System.IO.Path]::DirectorySeparatorChar
  $baseUri = New-Object System.Uri($ManifestDirectory.TrimEnd([char[]]@('/', '\')) + $separator)
  $pathUri = New-Object System.Uri($Path)
  return [System.Uri]::UnescapeDataString($baseUri.MakeRelativeUri($pathUri).ToString())
}

function ConvertTo-NativeArgument {
  param([AllowEmptyString()][string]$Value)

  if ($Value.Length -gt 0 -and $Value -notmatch '[\s"]') {
    return $Value
  }

  $builder = New-Object System.Text.StringBuilder
  [void]$builder.Append('"')
  $backslashes = 0
  foreach ($character in $Value.ToCharArray()) {
    if ($character -eq '\') {
      $backslashes += 1
      continue
    }

    if ($character -eq '"') {
      if ($backslashes -gt 0) {
        [void]$builder.Append((('\' * ($backslashes * 2)) -join ''))
      }
      [void]$builder.Append('\"')
      $backslashes = 0
      continue
    }

    if ($backslashes -gt 0) {
      [void]$builder.Append((('\' * $backslashes) -join ''))
      $backslashes = 0
    }
    [void]$builder.Append($character)
  }

  if ($backslashes -gt 0) {
    [void]$builder.Append((('\' * ($backslashes * 2)) -join ''))
  }
  [void]$builder.Append('"')
  return $builder.ToString()
}

function Stop-CapturedProcess {
  param([System.Diagnostics.Process]$Process)

  if ($Process.HasExited) {
    return
  }

  try {
    $Process.Kill($true)
    $Process.WaitForExit()
    return
  } catch {
    if ($Process.HasExited) {
      return
    }
  }

  if ($env:OS -eq "Windows_NT") {
    $taskkillPath = Join-Path $env:SystemRoot "System32/taskkill.exe"
    if (Test-Path -LiteralPath $taskkillPath -PathType Leaf) {
      $taskkillInfo = New-Object System.Diagnostics.ProcessStartInfo
      $taskkillInfo.FileName = $taskkillPath
      $taskkillInfo.UseShellExecute = $false
      $taskkillInfo.CreateNoWindow = $true
      $taskkillInfo.RedirectStandardOutput = $true
      $taskkillInfo.RedirectStandardError = $true
      $taskkillArguments = @("/PID", [string]$Process.Id, "/T", "/F")
      if ($null -ne $taskkillInfo.PSObject.Properties["ArgumentList"]) {
        foreach ($argument in $taskkillArguments) {
          [void]$taskkillInfo.ArgumentList.Add($argument)
        }
      } else {
        $taskkillInfo.Arguments = @($taskkillArguments | ForEach-Object { ConvertTo-NativeArgument $_ }) -join " "
      }

      $taskkill = New-Object System.Diagnostics.Process
      $taskkill.StartInfo = $taskkillInfo
      try {
        [void]$taskkill.Start()
        $taskkillStdout = $taskkill.StandardOutput.ReadToEndAsync()
        $taskkillStderr = $taskkill.StandardError.ReadToEndAsync()
        $taskkill.WaitForExit()
        $null = $taskkillStdout.GetAwaiter().GetResult()
        $null = $taskkillStderr.GetAwaiter().GetResult()
      } finally {
        $taskkill.Dispose()
      }
      if ($Process.WaitForExit(5000)) {
        return
      }
    }
  }

  $Process.Kill()
  $Process.WaitForExit()
}

function Get-Sha256 {
  param([string]$Path)

  $sha256 = [System.Security.Cryptography.SHA256]::Create()
  try {
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    $digest = $sha256.ComputeHash($bytes)
    return "sha256:$([System.BitConverter]::ToString($digest).Replace('-', '').ToLowerInvariant())"
  } finally {
    $sha256.Dispose()
  }
}

function Get-RunnerIdentity {
  $provider = if ($env:GITHUB_ACTIONS -eq "true") { "github-actions" } else { "local" }
  $name = if ([string]::IsNullOrWhiteSpace($env:RUNNER_NAME)) {
    [System.Environment]::MachineName
  } else {
    $env:RUNNER_NAME
  }
  $os = if ([string]::IsNullOrWhiteSpace($env:RUNNER_OS)) {
    if ($env:OS -eq "Windows_NT") { "Windows" } else { [System.Environment]::OSVersion.Platform.ToString() }
  } else {
    $env:RUNNER_OS
  }
  $architecture = if ([string]::IsNullOrWhiteSpace($env:RUNNER_ARCH)) {
    if ([string]::IsNullOrWhiteSpace($env:PROCESSOR_ARCHITECTURE)) { "unknown" } else { $env:PROCESSOR_ARCHITECTURE }
  } else {
    $env:RUNNER_ARCH
  }
  $identity = if ($provider -eq "github-actions") {
    "$name/$($env:GITHUB_RUN_ID)/$($env:GITHUB_RUN_ATTEMPT)/$($env:GITHUB_JOB)"
  } else {
    "$name/process-$PID"
  }

  return [ordered]@{
    provider = $provider
    identity = $identity
    name = $name
    os = $os
    architecture = $architecture
    run_id = if ([string]::IsNullOrWhiteSpace($env:GITHUB_RUN_ID)) { $null } else { $env:GITHUB_RUN_ID }
    run_attempt = if ([string]::IsNullOrWhiteSpace($env:GITHUB_RUN_ATTEMPT)) { $null } else { $env:GITHUB_RUN_ATTEMPT }
    job = if ([string]::IsNullOrWhiteSpace($env:GITHUB_JOB)) { $null } else { $env:GITHUB_JOB }
  }
}

$manifestFullPath = Resolve-OutputPath $ManifestPath
$manifestDirectory = [System.IO.Path]::GetDirectoryName($manifestFullPath)
if ([string]::IsNullOrWhiteSpace($manifestDirectory)) {
  throw "ManifestPath must resolve to a file path."
}

$manifestName = [System.IO.Path]::GetFileNameWithoutExtension($manifestFullPath)
if ([string]::IsNullOrWhiteSpace($StdoutPath)) {
  $StdoutPath = Join-Path $manifestDirectory "$manifestName.stdout"
}
if ([string]::IsNullOrWhiteSpace($StderrPath)) {
  $StderrPath = Join-Path $manifestDirectory "$manifestName.stderr"
}
$stdoutFullPath = Resolve-OutputPath $StdoutPath
$stderrFullPath = Resolve-OutputPath $StderrPath

Assert-ManifestChildPath $manifestDirectory $stdoutFullPath "StdoutPath"
Assert-ManifestChildPath $manifestDirectory $stderrFullPath "StderrPath"
if ($manifestFullPath -eq $stdoutFullPath -or $manifestFullPath -eq $stderrFullPath -or $stdoutFullPath -eq $stderrFullPath) {
  throw "ManifestPath, StdoutPath, and StderrPath must be distinct."
}

[System.IO.Directory]::CreateDirectory($manifestDirectory) | Out-Null
[System.IO.Directory]::CreateDirectory([System.IO.Path]::GetDirectoryName($stdoutFullPath)) | Out-Null
[System.IO.Directory]::CreateDirectory([System.IO.Path]::GetDirectoryName($stderrFullPath)) | Out-Null

$startInfo = New-Object System.Diagnostics.ProcessStartInfo
$startInfo.FileName = $Executable
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
$startInfo.RedirectStandardOutput = $true
$startInfo.RedirectStandardError = $true
$startInfo.StandardOutputEncoding = $Utf8NoBom
$startInfo.StandardErrorEncoding = $Utf8NoBom

if ($null -ne $startInfo.PSObject.Properties["ArgumentList"]) {
  foreach ($argument in @($ArgumentList)) {
    [void]$startInfo.ArgumentList.Add($argument)
  }
} else {
  $quotedArguments = @($ArgumentList | ForEach-Object { ConvertTo-NativeArgument $_ })
  $startInfo.Arguments = $quotedArguments -join " "
}

$startedAt = [System.DateTimeOffset]::UtcNow
$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
$outcome = "failure"
$exitCode = $null
$failureReason = $null
$stdout = ""
$stderr = ""
$process = New-Object System.Diagnostics.Process
$process.StartInfo = $startInfo
$processStarted = $false

try {
  [void]$process.Start()
  $processStarted = $true
  $stdoutTask = $process.StandardOutput.ReadToEndAsync()
  $stderrTask = $process.StandardError.ReadToEndAsync()

  if ($process.WaitForExit($TimeoutMilliseconds)) {
    $process.WaitForExit()
    $exitCode = [int]$process.ExitCode
    $outcome = if ($exitCode -eq 0) { "success" } else { "failure" }
    if ($outcome -eq "failure") {
      $failureReason = "command exited with exit_code=$exitCode"
    }
  } else {
    $outcome = "timeout"
    $failureReason = "command exceeded timeout_ms=$TimeoutMilliseconds"
    Stop-CapturedProcess $process
  }

  $stdout = $stdoutTask.GetAwaiter().GetResult()
  $stderr = $stderrTask.GetAwaiter().GetResult()
} catch {
  $outcome = "failure"
  $failureReason = $_.Exception.Message
  if ($processStarted -and -not $process.HasExited) {
    try {
      Stop-CapturedProcess $process
    } catch {
      $failureReason = "$failureReason; failed to terminate child: $($_.Exception.Message)"
    }
  }
} finally {
  $stopwatch.Stop()
  $process.Dispose()
}

$finishedAt = [System.DateTimeOffset]::UtcNow
[System.IO.File]::WriteAllText($stdoutFullPath, $stdout, $Utf8NoBom)
[System.IO.File]::WriteAllText($stderrFullPath, $stderr, $Utf8NoBom)

$manifest = [ordered]@{
  format = $CaptureFormat
  capture_id = $CaptureId
  executable = $Executable
  argv = @($ArgumentList)
  outcome = $outcome
  started_at = $startedAt.ToString("o")
  finished_at = $finishedAt.ToString("o")
  duration_ms = [int64][System.Math]::Ceiling($stopwatch.Elapsed.TotalMilliseconds)
  exit_code = $exitCode
  failure_reason = $failureReason
  runner = Get-RunnerIdentity
  stdout = [ordered]@{
    path = ConvertTo-ManifestRelativePath $manifestDirectory $stdoutFullPath
    byte_count = [System.IO.FileInfo]::new($stdoutFullPath).Length
    sha256 = Get-Sha256 $stdoutFullPath
  }
  stderr = [ordered]@{
    path = ConvertTo-ManifestRelativePath $manifestDirectory $stderrFullPath
    byte_count = [System.IO.FileInfo]::new($stderrFullPath).Length
    sha256 = Get-Sha256 $stderrFullPath
  }
}

$manifestJson = $manifest | ConvertTo-Json -Depth 8
[System.IO.File]::WriteAllText($manifestFullPath, "$manifestJson`n", $Utf8NoBom)
Write-Output $manifestFullPath

if (-not $NoFail -and $outcome -ne "success") {
  $exitDescription = if ($null -eq $exitCode) { "none" } else { [string]$exitCode }
  throw "[release-evidence-capture] outcome=$outcome exit_code=$exitDescription manifest=$manifestFullPath"
}
