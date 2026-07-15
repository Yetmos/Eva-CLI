[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$CaptureScript = Join-Path $PSScriptRoot "capture-release-evidence.ps1"
$ProducerScript = Join-Path $PSScriptRoot "write-release-platform-evidence.ps1"
$AggregatorScript = Join-Path $PSScriptRoot "aggregate-release-platform-evidence.ps1"
$RepositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$RunId = "987654321"
$RunAttempt = "1"

function Assert-True {
  param(
    [bool]$Condition,
    [string]$Message
  )

  if (-not $Condition) {
    throw "[release-platform-evidence-test] $Message"
  }
}

function Assert-Equal {
  param(
    [object]$Actual,
    [object]$Expected,
    [string]$Message
  )

  if ([string]$Actual -cne [string]$Expected) {
    throw "[release-platform-evidence-test] $Message actual='$Actual' expected='$Expected'"
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
    Assert-True $message.Contains("reason=$Reason") "Expected failure reason '$Reason', got: $message"
    return
  }
  throw "[release-platform-evidence-test] Expected failure reason '$Reason', but the action succeeded."
}

function Write-Utf8LfFile {
  param(
    [string]$Path,
    [string]$Text
  )

  $normalized = $Text.Replace("`r`n", "`n").Replace("`r", "`n")
  [System.IO.File]::WriteAllText($Path, $normalized, $Utf8NoBom)
}

function Write-JsonFile {
  param(
    [string]$Path,
    [object]$Value
  )

  $json = ($Value | ConvertTo-Json -Depth 16 -Compress).Replace("`r`n", "`n").Replace("`r", "`n")
  Write-Utf8LfFile $Path "$json`n"
}

function Read-JsonFile {
  param([string]$Path)

  return [System.IO.File]::ReadAllText($Path, $Utf8NoBom) | ConvertFrom-Json
}

function Get-Sha256 {
  param([byte[]]$Bytes)

  $sha256 = [System.Security.Cryptography.SHA256]::Create()
  try {
    $digest = $sha256.ComputeHash($Bytes)
    return "sha256:$([System.BitConverter]::ToString($digest).Replace('-', '').ToLowerInvariant())"
  } finally {
    $sha256.Dispose()
  }
}

function ConvertFrom-Hex {
  param([string]$Value)

  Assert-True ($Value.Length % 2 -eq 0) "bundle hex value has odd length"
  $bytes = New-Object byte[] ($Value.Length / 2)
  for ($index = 0; $index -lt $bytes.Length; $index += 1) {
    $bytes[$index] = [System.Convert]::ToByte($Value.Substring($index * 2, 2), 16)
  }
  return $bytes
}

function Get-BundlePayloadIdentity {
  param([string]$Manifest)

  $fields = @{}
  foreach ($line in $Manifest.Split([char]"`n")) {
    if ([string]::IsNullOrEmpty($line)) {
      continue
    }
    $separator = $line.IndexOf('=')
    Assert-True ($separator -gt 0) "bundle manifest line is not key=value"
    $key = $line.Substring(0, $separator)
    Assert-True (-not $fields.ContainsKey($key)) "bundle manifest field is duplicated"
    $fields[$key] = $line.Substring($separator + 1)
  }
  $entryCount = [int]$fields["entry_count"]
  $payload = New-Object System.IO.MemoryStream
  try {
    for ($index = 0; $index -lt $entryCount; $index += 1) {
      foreach ($suffix in @("subject_hex", "envelope_hex")) {
        [byte[]]$bytes = ConvertFrom-Hex ([string]$fields["entry.$index.$suffix"])
        [byte[]]$lengthBytes = [System.BitConverter]::GetBytes([uint64]$bytes.LongLength)
        if ([System.BitConverter]::IsLittleEndian) {
          [System.Array]::Reverse($lengthBytes)
        }
        $payload.Write($lengthBytes, 0, $lengthBytes.Length)
        $payload.Write($bytes, 0, $bytes.Length)
      }
    }
    return [pscustomobject]@{
      ClaimedDigest = [string]$fields["bundle_digest"]
      ActualDigest = Get-Sha256 $payload.ToArray()
    }
  } finally {
    $payload.Dispose()
  }
}

function Assert-BytesEqual {
  param(
    [string]$FirstPath,
    [string]$SecondPath,
    [string]$Message
  )

  $first = [System.IO.File]::ReadAllBytes($FirstPath)
  $second = [System.IO.File]::ReadAllBytes($SecondPath)
  Assert-Equal $first.LongLength $second.LongLength "$Message length"
  Assert-Equal (Get-Sha256 $first) (Get-Sha256 $second) "$Message digest"
}

function Remove-TestLink {
  param([string]$Path)

  if ($env:OS -eq "Windows_NT") {
    if ([System.IO.Directory]::Exists($Path)) {
      [System.IO.Directory]::Delete($Path)
    }
  } elseif (Test-Path -LiteralPath $Path) {
    Remove-Item -LiteralPath $Path -Force
  }
}

function Get-PlatformPolicy {
  param(
    [string]$Os,
    [string]$Architecture,
    [string]$Version
  )

  if ($Os -eq "Windows" -and $Architecture -eq "X64") {
    return [pscustomobject]@{ Target = "x86_64-pc-windows-msvc"; Archive = "eva-cli-$Version-x86_64-pc-windows-msvc.zip" }
  }
  if ($Os -eq "Linux" -and $Architecture -eq "X64") {
    return [pscustomobject]@{ Target = "x86_64-unknown-linux-gnu"; Archive = "eva-cli-$Version-x86_64-unknown-linux-gnu.tar.gz" }
  }
  if ($Os -eq "macOS" -and $Architecture -eq "ARM64") {
    return [pscustomobject]@{ Target = "aarch64-apple-darwin"; Archive = "eva-cli-$Version-aarch64-apple-darwin.tar.gz" }
  }
  throw "unsupported test platform $Os/$Architecture"
}

function Invoke-RealCapture {
  param(
    [string]$Executable,
    [string]$ManifestPath,
    [string]$CaptureId,
    [string]$RunnerOs,
    [string]$RunnerArchitecture,
    [string]$RunnerJob,
    [string]$Attempt
  )

  [System.Environment]::SetEnvironmentVariable("GITHUB_ACTIONS", "true")
  [System.Environment]::SetEnvironmentVariable("GITHUB_RUN_ID", $RunId)
  [System.Environment]::SetEnvironmentVariable("GITHUB_RUN_ATTEMPT", $Attempt)
  [System.Environment]::SetEnvironmentVariable("GITHUB_JOB", $RunnerJob)
  [System.Environment]::SetEnvironmentVariable("RUNNER_NAME", "contract-runner")
  [System.Environment]::SetEnvironmentVariable("RUNNER_OS", $RunnerOs)
  [System.Environment]::SetEnvironmentVariable("RUNNER_ARCH", $RunnerArchitecture)

  & $CaptureScript `
    -Executable $Executable `
    -ArgumentList @("--version") `
    -ManifestPath $ManifestPath `
    -CaptureId $CaptureId `
    -TimeoutMilliseconds 120000 | Out-Null
}

function New-RawPlatformFixture {
  param(
    [string]$Name,
    [string]$RunnerOs,
    [string]$RunnerArchitecture,
    [string]$Attempt = $RunAttempt
  )

  $root = Join-Path $TestRoot $Name
  $captureDirectory = Join-Path $root "captures"
  $archiveDirectory = Join-Path $root "archive"
  [System.IO.Directory]::CreateDirectory($captureDirectory) | Out-Null
  [System.IO.Directory]::CreateDirectory($archiveDirectory) | Out-Null
  $policy = Get-PlatformPolicy $RunnerOs $RunnerArchitecture $Version
  $smokePath = Join-Path $captureDirectory "smoke.json"
  $toolchainPath = Join-Path $captureDirectory "toolchain.json"
  $job = "native-$($policy.Target)"
  Invoke-RealCapture $EvaExecutable $smokePath "native-smoke" $RunnerOs $RunnerArchitecture $job $Attempt
  Invoke-RealCapture $RustcExecutable $toolchainPath "rust-toolchain" $RunnerOs $RunnerArchitecture $job $Attempt
  $archivePath = Join-Path $archiveDirectory $policy.Archive
  Write-Utf8LfFile $archivePath "archive:$($policy.Target):$SourceCommit`n"
  return [pscustomobject]@{
    Root = $root
    Manifest = Join-Path $root "platform.json"
    Smoke = $smokePath
    Toolchain = $toolchainPath
    Archive = $archivePath
    Os = $RunnerOs
    Architecture = $RunnerArchitecture
    Target = $policy.Target
  }
}

function Write-PlatformFixture {
  param([object]$Fixture)

  & $ProducerScript `
    -SmokeCapturePath $Fixture.Smoke `
    -ToolchainCapturePath $Fixture.Toolchain `
    -ArchivePath $Fixture.Archive `
    -ManifestPath $Fixture.Manifest `
    -ExpectedOs $Fixture.Os `
    -ExpectedArchitecture $Fixture.Architecture `
    -ExpectedReleaseTag $ReleaseTag `
    -ExpectedSourceCommit $SourceCommit `
    -ExpectedRunId $RunId `
    -ExpectedRunAttempt $RunAttempt | Out-Null
}

function Invoke-Aggregate {
  param(
    [string[]]$Manifests,
    [string]$OutputDirectory
  )

  [System.IO.Directory]::CreateDirectory($OutputDirectory) | Out-Null
  $bundleManifest = Join-Path $OutputDirectory "platform-bundle.json"
  $nativeArtifacts = Join-Path $OutputDirectory "native-artifacts.json"
  & $AggregatorScript `
    -PlatformManifestPath $Manifests `
    -BundleManifestPath $bundleManifest `
    -NativeArtifactsPath $nativeArtifacts `
    -ExpectedReleaseTag $ReleaseTag `
    -ExpectedSourceCommit $SourceCommit `
    -ExpectedRunId $RunId `
    -ExpectedRunAttempt $RunAttempt | Out-Null
  return [pscustomobject]@{
    Manifest = $bundleManifest
    Subject = Join-Path $OutputDirectory "platform-bundle.subject"
    Envelope = Join-Path $OutputDirectory "platform-bundle.envelope"
    Native = $nativeArtifacts
  }
}

function Copy-PlatformFixture {
  param(
    [object]$Fixture,
    [string]$Name
  )

  $destination = Join-Path $TestRoot $Name
  Copy-Item -LiteralPath $Fixture.Root -Destination $destination -Recurse
  $index = Read-JsonFile (Join-Path $destination "platform.json")
  return [pscustomobject]@{
    Root = $destination
    Manifest = Join-Path $destination "platform.json"
    Smoke = Join-Path $destination ([string]$index.captures.smoke.manifest_path)
    Toolchain = Join-Path $destination ([string]$index.captures.toolchain.manifest_path)
    Archive = Join-Path $destination ([string]$index.archive.path)
    Os = [string]$index.platform.os
    Architecture = [string]$index.platform.architecture
    Target = [string]$index.platform.target
  }
}

function Invoke-ProducerExpectFailure {
  param(
    [object]$Fixture,
    [string]$Reason
  )

  Assert-ThrowsReason {
    & $ProducerScript `
      -SmokeCapturePath $Fixture.Smoke `
      -ToolchainCapturePath $Fixture.Toolchain `
      -ArchivePath $Fixture.Archive `
      -ManifestPath $Fixture.Manifest `
      -ExpectedOs $Fixture.Os `
      -ExpectedArchitecture $Fixture.Architecture `
      -ExpectedReleaseTag $ReleaseTag `
      -ExpectedSourceCommit $SourceCommit `
      -ExpectedRunId $RunId `
      -ExpectedRunAttempt $RunAttempt | Out-Null
  } $Reason
}

function Invoke-AggregateExpectFailure {
  param(
    [string]$Manifest,
    [string]$Name,
    [string]$Reason
  )

  $output = Join-Path $TestRoot $Name
  [System.IO.Directory]::CreateDirectory($output) | Out-Null
  Assert-ThrowsReason {
    & $AggregatorScript `
      -PlatformManifestPath @($Manifest) `
      -BundleManifestPath (Join-Path $output "bundle.json") `
      -ExpectedReleaseTag $ReleaseTag `
      -ExpectedSourceCommit $SourceCommit `
      -ExpectedRunId $RunId `
      -ExpectedRunAttempt $RunAttempt | Out-Null
  } $Reason
}

foreach ($script in @($CaptureScript, $ProducerScript, $AggregatorScript)) {
  Assert-True ([System.IO.File]::Exists($script)) "Missing contract script: $script"
}
$ciWorkflow = [System.IO.File]::ReadAllText((Join-Path $RepositoryRoot ".github/workflows/ci.yml"), $Utf8NoBom)
$releaseWorkflow = [System.IO.File]::ReadAllText((Join-Path $RepositoryRoot ".github/workflows/release.yml"), $Utf8NoBom)
Assert-True $ciWorkflow.Contains("./scripts/test-release-platform-evidence.ps1") "CI does not run the platform evidence contract"
Assert-True $releaseWorkflow.Contains("./scripts/write-release-platform-evidence.ps1") "native jobs do not write platform evidence"
Assert-True $releaseWorkflow.Contains("./scripts/aggregate-release-platform-evidence.ps1") "publish job does not aggregate platform evidence"
Assert-True (-not $releaseWorkflow.Contains("Merge native artifact evidence")) "legacy unverified native evidence merge is still present"

$savedEnvironment = @{}
foreach ($name in @("GITHUB_ACTIONS", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_JOB", "RUNNER_NAME", "RUNNER_OS", "RUNNER_ARCH")) {
  $savedEnvironment[$name] = [System.Environment]::GetEnvironmentVariable($name)
}

$TestRoot = Join-Path ([System.IO.Path]::GetTempPath()) "eva-release-platform-$([guid]::NewGuid().ToString('N'))"
$linkPath = $null
try {
  [System.IO.Directory]::CreateDirectory($TestRoot) | Out-Null
  $executableName = if ($env:OS -eq "Windows_NT") { "eva.exe" } else { "eva" }
  $EvaExecutable = Join-Path $RepositoryRoot "target/debug/$executableName"
  if (-not [System.IO.File]::Exists($EvaExecutable)) {
    & cargo build --quiet --package eva-cli
    Assert-Equal $LASTEXITCODE 0 "cargo build -p eva-cli failed"
  }
  Assert-True ([System.IO.File]::Exists($EvaExecutable)) "eva debug executable is missing"
  $RustcExecutable = (Get-Command rustc -ErrorAction Stop).Source
  Assert-True ([System.IO.File]::Exists($RustcExecutable)) "rustc executable is missing"

  $versionOutput = @(& $EvaExecutable --version)
  Assert-Equal $LASTEXITCODE 0 "eva --version failed"
  Assert-True ($versionOutput.Count -gt 0) "eva --version emitted no output"
  Assert-True ([string]$versionOutput[0] -match '^eva ([0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?)$') "eva --version first line is invalid"
  $Version = $Matches[1]
  $ReleaseTag = "v$Version"
  $SourceCommit = (& git -C $RepositoryRoot rev-parse HEAD).Trim()
  Assert-Equal $LASTEXITCODE 0 "git rev-parse HEAD failed"
  Assert-True ($SourceCommit -cmatch '^[0-9a-f]{40}$') "source commit is not canonical"

  $windows = New-RawPlatformFixture "windows" "Windows" "X64"
  $linux = New-RawPlatformFixture "linux" "Linux" "X64"
  $macos = New-RawPlatformFixture "macos" "macOS" "ARM64"
  foreach ($fixture in @($windows, $linux, $macos)) {
    Write-PlatformFixture $fixture
  }

  $linuxIndex = Read-JsonFile $linux.Manifest
  $linuxSubjectPath = Join-Path $linux.Root ([string]$linuxIndex.subject.path)
  $linuxSubject = [System.IO.File]::ReadAllText($linuxSubjectPath, $Utf8NoBom)
  $subjectLines = $linuxSubject.Split([char]"`n", [System.StringSplitOptions]::RemoveEmptyEntries)
  Assert-Equal $subjectLines[0] "format=eva.release.platform_subject.v1" "platform subject format drifted from the Rust contract"
  Assert-Equal $subjectLines[1] "tag=$ReleaseTag" "platform subject tag field drifted"
  Assert-Equal $subjectLines[2] "commit=$SourceCommit" "platform subject commit field drifted"
  Assert-True $linuxSubject.Contains("artifact.digest=") "platform subject artifact digest field is missing"
  Assert-True $linuxSubject.Contains("capture.manifest_digest=") "platform subject smoke capture field is missing"
  Assert-True $linuxSubject.Contains("toolchain_capture.manifest_digest=") "platform subject toolchain capture field is missing"

  $forward = Invoke-Aggregate @($windows.Manifest, $linux.Manifest, $macos.Manifest) (Join-Path $TestRoot "aggregate-forward")
  $reverse = Invoke-Aggregate @($macos.Manifest, $linux.Manifest, $windows.Manifest) (Join-Path $TestRoot "aggregate-reverse")
  Assert-BytesEqual $forward.Subject $reverse.Subject "bundle subject must be input-order stable"
  Assert-BytesEqual $forward.Envelope $reverse.Envelope "bundle envelope must be input-order stable"
  Assert-BytesEqual $forward.Native $reverse.Native "native artifact output must be input-order stable"

  $bundleBytes = [System.IO.File]::ReadAllBytes($forward.Subject)
  Assert-True ($bundleBytes.Length -gt 3) "bundle subject is empty"
  Assert-True (-not ($bundleBytes[0] -eq 0xEF -and $bundleBytes[1] -eq 0xBB -and $bundleBytes[2] -eq 0xBF)) "bundle subject has a UTF-8 BOM"
  $bundleText = $Utf8NoBom.GetString($bundleBytes)
  Assert-True (-not $bundleText.Contains("`r")) "bundle subject contains CR instead of fixed LF"
  $bundleIndex = Read-JsonFile $forward.Manifest
  Assert-Equal $bundleIndex.entry_count 3 "bundle entry count changed"
  Assert-Equal $bundleIndex.subject.sha256 (Get-Sha256 $bundleBytes) "bundle manifest digest cannot be recomputed"
  $payloadIdentity = Get-BundlePayloadIdentity $bundleText
  Assert-Equal $payloadIdentity.ClaimedDigest $payloadIdentity.ActualDigest "bundle payload digest cannot be recomputed"
  Assert-Equal $bundleIndex.bundle_digest $payloadIdentity.ActualDigest "bundle index does not bind the canonical payload"
  Assert-Equal $bundleIndex.entries[0].target "aarch64-apple-darwin" "bundle entry 0 is not ordinal target order"
  Assert-Equal $bundleIndex.entries[1].target "x86_64-pc-windows-msvc" "bundle entry 1 is not ordinal target order"
  Assert-Equal $bundleIndex.entries[2].target "x86_64-unknown-linux-gnu" "bundle entry 2 is not ordinal target order"
  $native = Read-JsonFile $forward.Native
  Assert-Equal $native.status "published" "verified native artifacts must be published"
  Assert-Equal @($native.artifacts).Count 3 "legacy native artifact count changed"
  Assert-Equal $native.platform_bundle_digest $bundleIndex.bundle_digest "legacy output is not bound to the verified bundle"

  $sourceTamper = Copy-PlatformFixture $linux "tamper-source"
  $sourceIndex = Read-JsonFile $sourceTamper.Manifest
  $sourceIndex.source_commit = "0123456789abcdef0123456789abcdef01234567"
  Write-JsonFile $sourceTamper.Manifest $sourceIndex
  Invoke-AggregateExpectFailure $sourceTamper.Manifest "reject-source" "platform_source_commit_mismatch"

  $archiveTamper = Copy-PlatformFixture $windows "tamper-archive"
  $archiveBytes = [System.IO.File]::ReadAllBytes($archiveTamper.Archive)
  $archiveBytes[0] = $archiveBytes[0] -bxor 1
  [System.IO.File]::WriteAllBytes($archiveTamper.Archive, $archiveBytes)
  Invoke-AggregateExpectFailure $archiveTamper.Manifest "reject-archive" "platform_archive_digest_mismatch"

  $stdoutTamper = Copy-PlatformFixture $macos "tamper-stdout"
  $stdoutIndex = Read-JsonFile $stdoutTamper.Manifest
  $stdoutPath = Join-Path $stdoutTamper.Root ([string]$stdoutIndex.captures.smoke.stdout.path)
  $stdoutBytes = [System.IO.File]::ReadAllBytes($stdoutPath)
  $stdoutBytes[0] = $stdoutBytes[0] -bxor 1
  [System.IO.File]::WriteAllBytes($stdoutPath, $stdoutBytes)
  Invoke-AggregateExpectFailure $stdoutTamper.Manifest "reject-stdout" "platform_capture_stream_digest_mismatch"

  $attemptTamper = New-RawPlatformFixture "tamper-attempt" "Linux" "X64" "2"
  Invoke-ProducerExpectFailure $attemptTamper "platform_capture_run_attempt_mismatch"

  $identityTamper = New-RawPlatformFixture "tamper-runner-identity" "Linux" "X64"
  $identityCapture = Read-JsonFile $identityTamper.Smoke
  $identityCapture.runner.identity = "forged-runner/$RunId/$RunAttempt/$($identityCapture.runner.job)"
  Write-JsonFile $identityTamper.Smoke $identityCapture
  Invoke-ProducerExpectFailure $identityTamper "platform_capture_runner_invalid"

  $absolutePath = New-RawPlatformFixture "path-absolute" "Linux" "X64"
  $absoluteCapture = Read-JsonFile $absolutePath.Smoke
  $absoluteCapture.stdout.path = (Join-Path $TestRoot "outside.stdout")
  Write-JsonFile $absolutePath.Smoke $absoluteCapture
  Invoke-ProducerExpectFailure $absolutePath "platform_path_invalid"

  $parentPath = New-RawPlatformFixture "path-parent" "Linux" "X64"
  $parentCapture = Read-JsonFile $parentPath.Smoke
  $parentCapture.stdout.path = "../outside.stdout"
  Write-JsonFile $parentPath.Smoke $parentCapture
  Invoke-ProducerExpectFailure $parentPath "platform_path_invalid"

  $directoryPath = New-RawPlatformFixture "path-directory" "Linux" "X64"
  $directoryCapture = Read-JsonFile $directoryPath.Smoke
  [System.IO.Directory]::CreateDirectory((Join-Path ([System.IO.Path]::GetDirectoryName($directoryPath.Smoke)) "stream-directory")) | Out-Null
  $directoryCapture.stdout.path = "stream-directory"
  Write-JsonFile $directoryPath.Smoke $directoryCapture
  Invoke-ProducerExpectFailure $directoryPath "platform_path_not_file"

  $symlinkPath = New-RawPlatformFixture "path-symlink" "Linux" "X64"
  $symlinkCaptureDirectory = [System.IO.Path]::GetDirectoryName($symlinkPath.Smoke)
  $outsideDirectory = Join-Path $TestRoot "outside-streams"
  [System.IO.Directory]::CreateDirectory($outsideDirectory) | Out-Null
  Write-Utf8LfFile (Join-Path $outsideDirectory "escape.stdout") "eva $Version`n"
  $linkPath = Join-Path $symlinkCaptureDirectory "linked"
  if ($env:OS -eq "Windows_NT") {
    New-Item -ItemType Junction -Path $linkPath -Target $outsideDirectory | Out-Null
  } else {
    New-Item -ItemType SymbolicLink -Path $linkPath -Target $outsideDirectory | Out-Null
  }
  $symlinkCapture = Read-JsonFile $symlinkPath.Smoke
  $symlinkCapture.stdout.path = "linked/escape.stdout"
  Write-JsonFile $symlinkPath.Smoke $symlinkCapture
  Invoke-ProducerExpectFailure $symlinkPath "platform_path_symlink"
  Remove-TestLink $linkPath
  $linkPath = $null

  Write-Output "release platform evidence contract: passed"
} finally {
  if ($null -ne $linkPath -and (Test-Path -LiteralPath $linkPath)) {
    Remove-TestLink $linkPath
  }
  foreach ($name in $savedEnvironment.Keys) {
    [System.Environment]::SetEnvironmentVariable([string]$name, $savedEnvironment[$name])
  }
  $tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd([char[]]@('/', '\'))
  $resolvedTestRoot = [System.IO.Path]::GetFullPath($TestRoot)
  if ($resolvedTestRoot.StartsWith("$tempRoot$([System.IO.Path]::DirectorySeparatorChar)", [System.StringComparison]::OrdinalIgnoreCase) -and
      [System.IO.Path]::GetFileName($resolvedTestRoot).StartsWith("eva-release-platform-", [System.StringComparison]::Ordinal)) {
    Remove-Item -LiteralPath $resolvedTestRoot -Recurse -Force -ErrorAction SilentlyContinue
  }
}
