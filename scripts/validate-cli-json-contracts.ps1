[CmdletBinding()]
param(
  [string]$Eva,
  [string]$ContractRoot
)

$ErrorActionPreference = "Stop"

$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if ([string]::IsNullOrWhiteSpace($ContractRoot)) {
  $ContractRoot = Join-Path $Root "contracts/cli-json"
}
$ContractRoot = (Resolve-Path $ContractRoot).Path

function Fail {
  param([string]$Message)
  throw "[cli-json-contract] $Message"
}

function Convert-CommandArg {
  param([string]$Value)
  $Value.Replace("<repo>", $Root)
}

function Invoke-EvaJson {
  param([string[]]$CommandArgs)
  if ([string]::IsNullOrWhiteSpace($Eva)) {
    $output = & cargo run --quiet -- @CommandArgs
  } else {
    $output = & $Eva @CommandArgs
  }
  $exitCode = $LASTEXITCODE
  [pscustomobject]@{
    ExitCode = $exitCode
    Stdout = ($output -join "`n")
  }
}

function Get-JsonProperties {
  param($Value)
  @($Value.PSObject.Properties | Where-Object { $_.MemberType -eq "NoteProperty" })
}

function Test-IsJsonObject {
  param($Value)
  $null -ne $Value -and $Value -is [pscustomobject]
}

function Test-IsJsonArray {
  param($Value)
  $null -ne $Value -and $Value -is [System.Array]
}

function Test-ContractMatch {
  param(
    $Actual,
    $Expected
  )
  try {
    Assert-ContractSubset -Actual $Actual -Expected $Expected -Path "$"
    $true
  } catch {
    $false
  }
}

function Assert-ContractSubset {
  param(
    $Actual,
    $Expected,
    [string]$Path
  )

  if (Test-IsJsonObject $Expected) {
    $expectedProperties = Get-JsonProperties $Expected
    if ($expectedProperties.Count -eq 1 -and $expectedProperties[0].Name -eq "__contains") {
      if (-not ($Actual -is [string])) {
        Fail "$Path expected a string containing '$($expectedProperties[0].Value)'."
      }
      if (-not $Actual.Contains([string]$expectedProperties[0].Value)) {
        Fail "$Path did not contain '$($expectedProperties[0].Value)'."
      }
      return
    }

    if (-not (Test-IsJsonObject $Actual)) {
      Fail "$Path expected an object."
    }
    foreach ($property in $expectedProperties) {
      $actualProperty = $Actual.PSObject.Properties[$property.Name]
      if ($null -eq $actualProperty) {
        Fail "$Path.$($property.Name) is missing."
      }
      Assert-ContractSubset `
        -Actual $actualProperty.Value `
        -Expected $property.Value `
        -Path "$Path.$($property.Name)"
    }
    return
  }

  if (Test-IsJsonArray $Expected) {
    if (-not (Test-IsJsonArray $Actual)) {
      Fail "$Path expected an array."
    }
    foreach ($expectedItem in $Expected) {
      $matched = $false
      foreach ($actualItem in $Actual) {
        if (Test-ContractMatch -Actual $actualItem -Expected $expectedItem) {
          $matched = $true
          break
        }
      }
      if (-not $matched) {
        $itemJson = $expectedItem | ConvertTo-Json -Depth 100 -Compress
        Fail "$Path does not contain required item $itemJson."
      }
    }
    return
  }

  if ($null -eq $Expected) {
    if ($null -ne $Actual) {
      Fail "$Path expected null."
    }
    return
  }

  if ($Actual -ne $Expected) {
    Fail "$Path expected '$Expected' but found '$Actual'."
  }
}

$contractFiles = @(Get-ChildItem -LiteralPath $ContractRoot -Filter "*.json" | Sort-Object Name)
if ($contractFiles.Count -eq 0) {
  Fail "No contract fixture JSON files found under $ContractRoot."
}

$validated = 0
foreach ($file in $contractFiles) {
  $fixture = Get-Content -LiteralPath $file.FullName -Raw -Encoding utf8 | ConvertFrom-Json
  if ([string]::IsNullOrWhiteSpace($fixture.id)) {
    Fail "$($file.Name) must declare id."
  }
  if (-not (Test-IsJsonArray $fixture.command)) {
    Fail "$($file.Name) must declare command as an array."
  }
  $args = @($fixture.command | ForEach-Object { Convert-CommandArg ([string]$_) })
  $expectedExitCode = if ($null -eq $fixture.process_exit_code) { 0 } else { [int]$fixture.process_exit_code }
  $result = Invoke-EvaJson -CommandArgs $args
  if ($result.ExitCode -ne $expectedExitCode) {
    Fail "$($fixture.id) process exit code was $($result.ExitCode), expected $expectedExitCode."
  }
  try {
    $actualJson = $result.Stdout | ConvertFrom-Json
  } catch {
    Fail "$($fixture.id) did not emit valid JSON stdout: $($result.Stdout)"
  }
  Assert-ContractSubset -Actual $actualJson -Expected $fixture.expected -Path "$($fixture.id)"
  $validated += 1
}

Write-Host "CLI JSON contracts validated: $validated fixture(s)."
