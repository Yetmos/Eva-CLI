[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)][ValidateSet('homebrew','winget','apt')][string]$Manager,
  [Parameter(Mandatory = $true)][string]$MetadataPath,
  [switch]$RequireNativeTool,
  [switch]$StaticOnly,
  [string]$NativePackageName
)
$ErrorActionPreference = 'Stop'
$path = [System.IO.Path]::GetFullPath($MetadataPath)
$validationPath = $path
if ($Manager -eq 'winget' -and [System.IO.Directory]::Exists($path)) {
  $files = @([System.IO.Directory]::GetFiles($path, '*.yaml'))
  if ($files.Count -ne 3) { throw "winget_manifest_count_invalid:$($files.Count)" }
  [System.Array]::Sort($files, [System.StringComparer]::Ordinal)
  $bytes = [System.Text.UTF8Encoding]::new($false).GetBytes((($files | ForEach-Object { [System.IO.File]::ReadAllText($_) }) -join "`n"))
} else {
  if (-not [System.IO.File]::Exists($path)) { throw "metadata_missing:$path" }
  $bytes = [System.IO.File]::ReadAllBytes($path)
}
if ($bytes.Length -eq 0 -or $bytes.Length -gt 1048576) { throw "metadata_size_invalid:$($bytes.Length)" }
$text = [System.Text.UTF8Encoding]::new($false,$true).GetString($bytes)
if ($text.Contains([char]0) -or $text.Contains([char]13)) { throw 'metadata_encoding_invalid' }
$tool = $null; $arguments = @(); $staticValid = $false
switch ($Manager) {
  'homebrew' { $staticValid = $text -match '^class EvaCli < Formula' -and $text -match '(?m)^  url "https://' -and $text -match '(?m)^  sha256 "[0-9a-f]{64}"$'; $tool = Get-Command brew -ErrorAction SilentlyContinue; $arguments = @('audit','--strict','--formula',$(if ($NativePackageName) { $NativePackageName } else { $path })) }
  'winget' { $staticValid = ([regex]::Matches($text, '(?m)^PackageIdentifier: Yetmos\.EvaCLI$').Count -eq 3) -and ([regex]::Matches($text, '(?m)^ManifestVersion: 1\.6\.0$').Count -eq 3); $tool = Get-Command winget -ErrorAction SilentlyContinue; $arguments = @('validate','--manifest',$validationPath,'--disable-interactivity') }
  'apt' { $staticValid = $text -match '(?m)^Package: eva-cli$' -and $text -match '(?m)^Version: ' -and $text -match '(?m)^Architecture: amd64$'; $tool = Get-Command apt-ftparchive -ErrorAction SilentlyContinue; $arguments = @('packages',(Split-Path -Parent $path)) }
}
if (-not $staticValid) { throw "metadata_static_validation_failed:$Manager" }
if ($StaticOnly) { [ordered]@{ manager=$Manager; status='static_passed'; static_valid=$true; native_tool=$null; metadata_path=$path; size_bytes=$bytes.Length } | ConvertTo-Json -Compress; exit 0 }
if ($null -eq $tool) { [ordered]@{ manager=$Manager; status='unavailable'; static_valid=$true; native_tool=$null; metadata_path=$path; size_bytes=$bytes.Length } | ConvertTo-Json -Compress; if ($RequireNativeTool) { exit 3 }; exit 0 }
$nativeOutput = @(& $tool.Source @arguments 2>&1)
$nativeOutput | ForEach-Object { Write-Host $_ }
if ($LASTEXITCODE -ne 0) {
  if ($env:GITHUB_ACTIONS -eq 'true') {
    $annotation = (($nativeOutput | Select-Object -Last 40) -join "`n").Replace('%','%25').Replace("`r",'%0D').Replace("`n",'%0A')
    Write-Output "::error title=$Manager native validator failed::$annotation"
  }
  throw ('native_validator_failed:' + $Manager + ':' + $LASTEXITCODE)
}
[ordered]@{ manager=$Manager; status='passed'; static_valid=$true; native_tool=$tool.Source; metadata_path=$path; size_bytes=$bytes.Length } | ConvertTo-Json -Compress
