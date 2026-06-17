#!/usr/bin/env pwsh
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$ManifestPath = Join-Path $Root "docs/_i18n/manifest.json"
$LocaleRoot = Join-Path $Root "website/_i18n"
$WebsiteRoot = Join-Path $Root "website"
$DocsRoot = Join-Path $Root "docs"

function Read-JsonFile {
  param([Parameter(Mandatory = $true)][string]$Path)
  return Get-Content -Raw -Encoding UTF8 -LiteralPath $Path | ConvertFrom-Json
}

function Fail {
  param([Parameter(Mandatory = $true)][string]$Message)
  throw "i18n validation failed: $Message"
}

function Test-RepoPath {
  param([Parameter(Mandatory = $true)][string]$RelativePath)
  $path = Join-Path $Root ($RelativePath -replace "/", [System.IO.Path]::DirectorySeparatorChar)
  return Test-Path -LiteralPath $path
}

function Get-PropertyValue {
  param(
    [Parameter(Mandatory = $true)]$Object,
    [Parameter(Mandatory = $true)][string]$Name
  )

  $property = $Object.PSObject.Properties | Where-Object { $_.Name -eq $Name } | Select-Object -First 1
  if ($null -eq $property) {
    return $null
  }

  return $property.Value
}

if (-not (Test-Path -LiteralPath $ManifestPath)) {
  Fail "Missing docs/_i18n/manifest.json."
}

$manifest = Read-JsonFile -Path $ManifestPath
if ([string]::IsNullOrWhiteSpace($manifest.defaultLocale)) {
  Fail "manifest.defaultLocale is required."
}

if ([string]::IsNullOrWhiteSpace($manifest.siteUrl)) {
  Fail "manifest.siteUrl is required for canonical URLs."
}

$locales = @($manifest.locales | Where-Object { $_.enabled -ne $false })
if ($locales.Count -eq 0) {
  Fail "At least one enabled locale is required."
}

$defaultLocale = @($locales | Where-Object { $_.code -eq $manifest.defaultLocale }) | Select-Object -First 1
if ($null -eq $defaultLocale) {
  Fail "Default locale '$($manifest.defaultLocale)' is not present in enabled locales."
}

foreach ($locale in $locales) {
  foreach ($field in @("code", "nativeLabel", "dir", "tier")) {
    if ([string]::IsNullOrWhiteSpace($locale.$field)) {
      Fail "Locale entry is missing '$field'."
    }
  }

  if ($locale.dir -notin @("ltr", "rtl")) {
    Fail "Locale '$($locale.code)' has invalid dir '$($locale.dir)'."
  }

  $localeFile = Join-Path $LocaleRoot "$($locale.code).json"
  if (-not (Test-Path -LiteralPath $localeFile)) {
    Fail "Missing website locale JSON for '$($locale.code)'."
  }

  $localeData = Read-JsonFile -Path $localeFile
  foreach ($path in @("meta", "brand", "nav", "home", "docsIndex", "feedback", "footer")) {
    if ($null -eq (Get-PropertyValue -Object $localeData -Name $path)) {
      Fail "website/_i18n/$($locale.code).json missing '$path'."
    }
  }

  $homePath = if ($locale.code -eq $manifest.defaultLocale) {
    Join-Path $WebsiteRoot "index.html"
  } else {
    Join-Path $WebsiteRoot "$($locale.code)/index.html"
  }

  if (-not (Test-Path -LiteralPath $homePath)) {
    Fail "Missing generated home page for locale '$($locale.code)': $homePath"
  }

  $homeHtml = Get-Content -Raw -Encoding UTF8 -LiteralPath $homePath
  if ($homeHtml -notmatch "<html lang=`"$([regex]::Escape($locale.code))`" dir=`"$([regex]::Escape($locale.dir))`"") {
    Fail "Generated home page for '$($locale.code)' has incorrect html lang/dir."
  }
  if ($homeHtml -notmatch 'rel="canonical"') {
    Fail "Generated home page for '$($locale.code)' is missing canonical link."
  }
  if ($homeHtml -notmatch 'hreflang="x-default"') {
    Fail "Generated home page for '$($locale.code)' is missing x-default hreflang."
  }
}

foreach ($document in $manifest.documents) {
  if ([string]::IsNullOrWhiteSpace($document.id)) {
    Fail "A document entry is missing id."
  }
  if ([string]::IsNullOrWhiteSpace($document.source)) {
    Fail "Document '$($document.id)' is missing source."
  }
  if (-not (Test-RepoPath -RelativePath $document.source)) {
    Fail "Document '$($document.id)' source does not exist: $($document.source)"
  }

  foreach ($locale in $locales) {
    if ($locale.code -eq $manifest.defaultLocale) {
      continue
    }

    $status = Get-PropertyValue -Object $document.status -Name $locale.code
    if ([string]::IsNullOrWhiteSpace($status)) {
      Fail "Document '$($document.id)' is missing translation status for '$($locale.code)'."
    }

    if ($status -notin @("current", "needs-review", "stale", "missing", "partial")) {
      Fail "Document '$($document.id)' has invalid status '$status' for '$($locale.code)'."
    }

    $translation = Get-PropertyValue -Object $document.translations -Name $locale.code
    if ($status -ne "missing") {
      if ([string]::IsNullOrWhiteSpace($translation)) {
        Fail "Document '$($document.id)' has status '$status' but no translation path for '$($locale.code)'."
      }
      if (-not (Test-RepoPath -RelativePath $translation)) {
        Fail "Document '$($document.id)' translation does not exist for '$($locale.code)': $translation"
      }
    }
  }
}

$docsIndexPath = Join-Path $WebsiteRoot "docs/index.html"
if (-not (Test-Path -LiteralPath $docsIndexPath)) {
  Fail "Missing generated website/docs/index.html."
}

$docsIndexHtml = Get-Content -Raw -Encoding UTF8 -LiteralPath $docsIndexPath
if ($docsIndexHtml -notmatch '<html lang="en" dir="ltr"') {
  Fail "website/docs/index.html must be the English docs entry."
}
if ($docsIndexHtml -notmatch 'rel="canonical"') {
  Fail "website/docs/index.html is missing canonical link."
}

$unexpectedDocsChildren = Get-ChildItem -LiteralPath (Join-Path $WebsiteRoot "docs") -Force |
  Where-Object { $_.Name -ne "index.html" }
if ($unexpectedDocsChildren) {
  $names = ($unexpectedDocsChildren | ForEach-Object { $_.Name }) -join ", "
  Fail "website/docs must only contain index.html to avoid docs/ publish overwrite conflicts. Found: $names"
}

if (-not (Test-Path -LiteralPath (Join-Path $DocsRoot "en"))) {
  Fail "Missing docs/en directory."
}
if (-not (Test-Path -LiteralPath (Join-Path $DocsRoot "zh-CN"))) {
  Fail "Missing docs/zh-CN directory."
}

Write-Host "i18n structure validated for $($locales.Count) locale(s) and $($manifest.documents.Count) document(s)."
