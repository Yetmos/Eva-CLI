$ErrorActionPreference = 'Stop'
$root = Join-Path ([System.IO.Path]::GetTempPath()) ('eva-package-validator-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null
try {
  $formula = Join-Path $root 'eva-cli.rb'
  $winget = Join-Path $root 'winget'
  $apt = Join-Path $root 'Packages'
  $lf = [char]10
  [System.IO.File]::WriteAllText($formula, ('class EvaCli < Formula' + $lf + '  url "https://example.test/eva.tar.gz"' + $lf + '  sha256 "' + ('a' * 64) + '"' + $lf + 'end' + $lf), [System.Text.UTF8Encoding]::new($false))
  New-Item -ItemType Directory -Path $winget | Out-Null
  foreach ($name in @('Yetmos.EvaCLI.yaml','Yetmos.EvaCLI.installer.yaml','Yetmos.EvaCLI.locale.en-US.yaml')) { [System.IO.File]::WriteAllText((Join-Path $winget $name), ('PackageIdentifier: Yetmos.EvaCLI' + $lf + 'ManifestVersion: 1.6.0' + $lf), [System.Text.UTF8Encoding]::new($false)) }
  [System.IO.File]::WriteAllText($apt, ('Package: eva-cli' + $lf + 'Version: 1.11.5-alpha' + $lf + 'Architecture: amd64' + $lf), [System.Text.UTF8Encoding]::new($false))
  foreach ($case in @(@('homebrew',$formula),@('winget',$winget),@('apt',$apt))) {
    $output = & (Join-Path $PSScriptRoot 'validate-package-manager-metadata.ps1') -Manager $case[0] -MetadataPath $case[1] -StaticOnly
    if ($LASTEXITCODE -ne 0) { throw "validator_failed:$($case[0])" }
    $json = $output | Select-Object -Last 1 | ConvertFrom-Json
    if (-not $json.static_valid -or $json.manager -ne $case[0]) { throw "validator_output_invalid:$($case[0])" }
  }
  [System.IO.File]::WriteAllText($apt, ('Package: wrong' + $lf), [System.Text.UTF8Encoding]::new($false))
  $failed = $false
  try { & (Join-Path $PSScriptRoot 'validate-package-manager-metadata.ps1') -Manager apt -MetadataPath $apt 2>$null | Out-Null } catch { $failed = $true }
  if (-not $failed) { throw 'invalid_metadata_was_accepted' }
  Write-Output '{"status":"passed","cases":4}'
} finally { Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue }
