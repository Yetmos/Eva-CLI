[CmdletBinding()]
param(
  [string]$RepositoryRoot = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($RepositoryRoot)) {
  $RepositoryRoot = Split-Path -Parent $PSScriptRoot
}
$RepositoryRoot = [System.IO.Path]::GetFullPath($RepositoryRoot)
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false, $true)
$CanaryEnvironment = "EVA_CREDENTIAL_LEAK_CANARY"

function Fail-CredentialHarness {
  param([string]$Reason, [string]$Detail)

  $safe = if ([string]::IsNullOrWhiteSpace($Detail)) { "none" } else { $Detail.Replace("`r", " ").Replace("`n", " ") }
  throw "[credential-leak-scan] reason=$Reason detail=$safe"
}

function Test-ByteSequence {
  param(
    [byte[]]$Bytes,
    [byte[]]$Pattern
  )

  if ($Pattern.Length -eq 0 -or $Bytes.Length -lt $Pattern.Length) {
    return $false
  }
  $last = $Bytes.Length - $Pattern.Length
  for ($offset = 0; $offset -le $last; $offset += 1) {
    if ($Bytes[$offset] -ne $Pattern[0]) {
      continue
    }
    $matched = $true
    for ($index = 1; $index -lt $Pattern.Length; $index += 1) {
      if ($Bytes[$offset + $index] -ne $Pattern[$index]) {
        $matched = $false
        break
      }
    }
    if ($matched) {
      return $true
    }
  }
  return $false
}

function Test-FileContainsCanary {
  param(
    [string]$Path,
    [string]$Canary
  )

  $bytes = [System.IO.File]::ReadAllBytes($Path)
  return (Test-ByteSequence -Bytes $bytes -Pattern ([System.Text.Encoding]::UTF8.GetBytes($Canary))) -or
    (Test-ByteSequence -Bytes $bytes -Pattern ([System.Text.Encoding]::Unicode.GetBytes($Canary))) -or
    (Test-ByteSequence -Bytes $bytes -Pattern ([System.Text.Encoding]::BigEndianUnicode.GetBytes($Canary)))
}

function Find-CredentialCanary {
  param(
    [string]$Root,
    [string]$Canary
  )

  if (-not (Test-Path -LiteralPath $Root -PathType Container)) {
    return $null
  }
  foreach ($file in @(Get-ChildItem -LiteralPath $Root -Recurse -Force -File)) {
    if (Test-FileContainsCanary -Path $file.FullName -Canary $Canary) {
      return $file.FullName
    }
  }
  return $null
}

function Assert-NoCredentialCanary {
  param(
    [string]$Root,
    [string]$Canary
  )

  $leak = Find-CredentialCanary -Root $Root -Canary $Canary
  if ($null -ne $leak) {
    Fail-CredentialHarness "credential_canary_detected" ([System.IO.Path]::GetFileName($leak))
  }
}

function Invoke-CredentialCase {
  param(
    [string]$Name,
    [string]$Filter,
    [string]$Root,
    [string]$Canary,
    [string]$Cargo
  )

  if ($Name -cnotmatch '^[a-z0-9-]+$') {
    Fail-CredentialHarness "case_name_invalid" $Name
  }
  $caseRoot = Join-Path $Root $Name
  [System.IO.Directory]::CreateDirectory($caseRoot) | Out-Null
  $stdoutPath = Join-Path $caseRoot "stdout.log"
  $stderrPath = Join-Path $caseRoot "stderr.log"
  $arguments = @(
    "test", "-p", "eva-adapter", $Filter,
    "--", "--exact", "--test-threads=1", "--nocapture"
  )

  $previousPreference = $ErrorActionPreference
  try {
    $ErrorActionPreference = "Continue"
    & $Cargo @arguments 1> $stdoutPath 2> $stderrPath
    $exitCode = $LASTEXITCODE
  } finally {
    $ErrorActionPreference = $previousPreference
  }
  Assert-NoCredentialCanary -Root $caseRoot -Canary $Canary
  if ($exitCode -ne 0) {
    Fail-CredentialHarness "credential_case_failed" $Name
  }
}

$cargoCommand = Get-Command cargo -CommandType Application -ErrorAction Stop
$cargoPath = $cargoCommand.Source
$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) "eva-credential-leak-scan-$([System.Guid]::NewGuid().ToString('N'))"
$canary = "eva-test-credential-canary-$([System.Guid]::NewGuid().ToString('N'))"
$hadPreviousCanary = Test-Path "Env:$CanaryEnvironment"
$previousCanary = if ($hadPreviousCanary) { [System.Environment]::GetEnvironmentVariable($CanaryEnvironment) } else { $null }
$locationPushed = $false

try {
  [System.IO.Directory]::CreateDirectory($testRoot) | Out-Null
  [System.Environment]::SetEnvironmentVariable($CanaryEnvironment, $canary)

  $negativeControls = @(
    @{ Name = "utf8"; Encoding = $Utf8NoBom },
    @{ Name = "utf16le"; Encoding = [System.Text.Encoding]::Unicode },
    @{ Name = "utf16be"; Encoding = [System.Text.Encoding]::BigEndianUnicode }
  )
  foreach ($control in $negativeControls) {
    $negativeRoot = Join-Path $testRoot "scanner-negative-$([string]$control.Name)"
    $negativePath = Join-Path $negativeRoot "deliberate-leak.txt"
    [System.IO.Directory]::CreateDirectory($negativeRoot) | Out-Null
    [System.IO.File]::WriteAllText($negativePath, "negative-control:$canary", $control.Encoding)
    $detected = $false
    try {
      Assert-NoCredentialCanary -Root $negativeRoot -Canary $canary
    } catch {
      if ($_.Exception.Message.Contains($canary)) {
        Fail-CredentialHarness "scanner_error_leaked_canary" ([string]$control.Name)
      }
      if ($_.Exception.Message.StartsWith("[credential-leak-scan] reason=credential_canary_detected ", [System.StringComparison]::Ordinal)) {
        $detected = $true
      } else {
        throw
      }
    }
    if (-not $detected) {
      Fail-CredentialHarness "scanner_false_negative" ([string]$control.Name)
    }
    Remove-Item -LiteralPath $negativeRoot -Recurse -Force
  }

  Push-Location $RepositoryRoot
  $locationPushed = $true
  $cases = @(
    @{ Name = "vault-memory-redaction"; Filter = "credential_vault::tests::memory_session_fetch_inject_release_and_debug_redact" },
    @{ Name = "stdio-stream-redaction"; Filter = "transports::stdio::tests::runner_redacts_injected_env_from_output_streams" },
    @{ Name = "stdio-runtime-redaction"; Filter = "runtime::tests::runtime_invokes_stdio_adapter_with_redacted_env" },
    @{ Name = "http-runtime-redaction"; Filter = "runtime::tests::runtime_invokes_http_adapter_and_redacts_credential_header" },
    @{ Name = "skill-artifact-redaction"; Filter = "transports::skill::tests::process_skill_runner_collects_artifacts_and_redacts_env" },
    @{ Name = "vault-default-fail-closed"; Filter = "credential_vault::tests::fail_closed_vault_never_reads_parent_environment" },
    @{ Name = "vault-missing-scope"; Filter = "credential_vault::tests::missing_scope_is_rejected_before_vault_access" },
    @{ Name = "vault-error-sanitized"; Filter = "credential_vault::tests::vault_errors_are_sanitized_and_failed_release_is_retried" }
  )
  foreach ($case in $cases) {
    Invoke-CredentialCase `
      -Name ([string]$case.Name) `
      -Filter ([string]$case.Filter) `
      -Root $testRoot `
      -Canary $canary `
      -Cargo $cargoPath
  }
  Assert-NoCredentialCanary -Root $testRoot -Canary $canary
  Write-Host "Credential leak scan harness passed: 8 credential cases and 3 scanner negative controls."
} finally {
  if ($locationPushed) {
    Pop-Location
  }
  if ($hadPreviousCanary) {
    [System.Environment]::SetEnvironmentVariable($CanaryEnvironment, $previousCanary)
  } else {
    [System.Environment]::SetEnvironmentVariable($CanaryEnvironment, $null)
  }
  if (Test-Path -LiteralPath $testRoot) {
    Remove-Item -LiteralPath $testRoot -Recurse -Force
  }
}
