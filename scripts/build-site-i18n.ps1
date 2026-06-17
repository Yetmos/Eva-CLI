#!/usr/bin/env pwsh
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$ManifestPath = Join-Path $Root "docs/_i18n/manifest.json"
$TemplateRoot = Join-Path $Root "website/_templates"
$LocaleRoot = Join-Path $Root "website/_i18n"
$WebsiteRoot = Join-Path $Root "website"

function Read-JsonFile {
  param([Parameter(Mandatory = $true)][string]$Path)
  return Get-Content -Raw -Encoding UTF8 -LiteralPath $Path | ConvertFrom-Json
}

function Write-Utf8NoBom {
  param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$Content
  )

  $directory = Split-Path -Parent $Path
  if ($directory) {
    New-Item -ItemType Directory -Force -Path $directory | Out-Null
  }

  $encoding = New-Object System.Text.UTF8Encoding($false)
  [System.IO.File]::WriteAllText($Path, $Content, $encoding)
}

function Html {
  param([AllowNull()][string]$Value)
  if ($null -eq $Value) {
    return ""
  }

  return [System.Net.WebUtility]::HtmlEncode($Value)
}

function Apply-Template {
  param(
    [Parameter(Mandatory = $true)][string]$Template,
    [Parameter(Mandatory = $true)][hashtable]$Tokens
  )

  $output = $Template
  foreach ($key in $Tokens.Keys) {
    $output = $output.Replace("{{$key}}", [string]$Tokens[$key])
  }

  if ($output -match "{{[^}]+}}") {
    throw "Template still contains an unresolved token: $($Matches[0])"
  }

  return $output
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

function Get-Document {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$Id
  )

  $document = @($Manifest.documents | Where-Object { $_.id -eq $Id }) | Select-Object -First 1
  if ($null -eq $document) {
    throw "Document '$Id' is not registered in docs/_i18n/manifest.json."
  }

  return $document
}

function Get-DocumentPath {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$DocumentId,
    [Parameter(Mandatory = $true)][string]$LocaleCode
  )

  $document = Get-Document -Manifest $Manifest -Id $DocumentId
  if ($LocaleCode -eq $Manifest.defaultLocale) {
    return $document.source
  }

  $translation = Get-PropertyValue -Object $document.translations -Name $LocaleCode
  if ([string]::IsNullOrWhiteSpace($translation)) {
    return $document.source
  }

  return $translation
}

function Convert-DocPathToRelativeHref {
  param(
    [Parameter(Mandatory = $true)][string]$DocPath,
    [Parameter(Mandatory = $true)][ValidateSet("root", "locale-home", "docs-index")][string]$Context
  )

  switch ($Context) {
    "root" { return $DocPath }
    "locale-home" { return "../$DocPath" }
    "docs-index" { return ($DocPath -replace "^docs/", "") }
  }
}

function Join-SiteUrl {
  param(
    [Parameter(Mandatory = $true)][string]$BaseUrl,
    [Parameter(Mandatory = $true)][string]$Path
  )

  $base = $BaseUrl.TrimEnd("/")
  if ($Path.StartsWith("/")) {
    return "$base$Path"
  }

  return "$base/$Path"
}

function Get-LocaleHomePath {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$LocaleCode
  )

  if ($LocaleCode -eq $Manifest.defaultLocale) {
    return "/"
  }

  return "/$LocaleCode/"
}

function New-AlternateLinks {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)]$Locales,
    [Parameter(Mandatory = $true)][ValidateSet("home", "docs-index")][string]$PageKind
  )

  $links = New-Object System.Collections.Generic.List[string]
  foreach ($locale in $Locales) {
    $code = [string]$locale.code
    if ($PageKind -eq "home") {
      $path = Get-LocaleHomePath -Manifest $Manifest -LocaleCode $code
    } elseif ($code -eq $Manifest.defaultLocale) {
      $path = "/docs/"
    } else {
      $readmePath = Get-DocumentPath -Manifest $Manifest -DocumentId "readme" -LocaleCode $code
      $path = "/$readmePath"
    }

    $href = Html (Join-SiteUrl -BaseUrl $Manifest.siteUrl -Path $path)
    $links.Add("    <link rel=`"alternate`" hreflang=`"$(Html $code)`" href=`"$href`">")
  }

  $defaultHref = if ($PageKind -eq "home") {
    Join-SiteUrl -BaseUrl $Manifest.siteUrl -Path "/"
  } else {
    Join-SiteUrl -BaseUrl $Manifest.siteUrl -Path "/docs/"
  }
  $links.Add("    <link rel=`"alternate`" hreflang=`"x-default`" href=`"$(Html $defaultHref)`">")
  return ($links -join "`n")
}

function New-LanguageSwitch {
  param(
    [Parameter(Mandatory = $true)]$Locales,
    [Parameter(Mandatory = $true)][hashtable]$HrefByLocale,
    [Parameter(Mandatory = $true)][string]$CurrentLocale
  )

  $items = New-Object System.Collections.Generic.List[string]
  foreach ($locale in $Locales) {
    $code = [string]$locale.code
    $current = if ($code -eq $CurrentLocale) { " aria-current=`"page`"" } else { "" }
    $href = Html $HrefByLocale[$code]
    $items.Add("          <a href=`"$href`" hreflang=`"$(Html $code)`" lang=`"$(Html $code)`"$current>$(Html $locale.nativeLabel)</a>")
  }

  return @"
      <div class="language-switch" aria-label="Language">
        <span>Language</span>
        <div class="language-links">
$($items -join "`n")
        </div>
      </div>
"@
}

function New-NavLinks {
  param(
    [Parameter(Mandatory = $true)]$LocaleData,
    [Parameter(Mandatory = $true)][string]$HomeHref,
    [Parameter(Mandatory = $true)][string]$DocsHref,
    [Parameter(Mandatory = $true)][string]$ArchitectureHref,
    [Parameter(Mandatory = $true)][string]$FeedbackHref
  )

  return @"
        <a href="$(Html $HomeHref)">$(Html $LocaleData.nav.home)</a>
        <a href="$(Html $DocsHref)">$(Html $LocaleData.nav.docs)</a>
        <a href="$(Html $ArchitectureHref)">$(Html $LocaleData.nav.architecture)</a>
        <a href="$(Html $FeedbackHref)">$(Html $LocaleData.nav.feedback)</a>
        <a href="https://github.com/Yetmos/Eva-CLI">GitHub</a>
"@
}

function New-HomeDocCards {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)]$Cards,
    [Parameter(Mandatory = $true)][string]$LocaleCode,
    [Parameter(Mandatory = $true)][ValidateSet("root", "locale-home")][string]$Context
  )

  $items = New-Object System.Collections.Generic.List[string]
  foreach ($card in $Cards) {
    $docPath = Get-DocumentPath -Manifest $Manifest -DocumentId $card.id -LocaleCode $LocaleCode
    $href = Convert-DocPathToRelativeHref -DocPath $docPath -Context $Context
    $items.Add(@"
          <a class="doc-link" href="$(Html $href)">
            <span>$(Html $card.title)</span>
            <small>$(Html $card.description)</small>
          </a>
"@)
  }

  return ($items -join "`n")
}

function New-FeedbackOptions {
  param([Parameter(Mandatory = $true)]$Options)

  $items = New-Object System.Collections.Generic.List[string]
  foreach ($option in $Options) {
    $items.Add("                <option value=`"$(Html $option)`">$(Html $option)</option>")
  }

  return ($items -join "`n")
}

function New-LanguageList {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)]$Locales
  )

  $items = New-Object System.Collections.Generic.List[string]
  foreach ($locale in $Locales) {
    $code = [string]$locale.code
    $readmePath = Get-DocumentPath -Manifest $Manifest -DocumentId "readme" -LocaleCode $code
    $href = Convert-DocPathToRelativeHref -DocPath $readmePath -Context "docs-index"
    $label = if ($code -eq $Manifest.defaultLocale) { "Canonical source" } else { "Translation: $($locale.coverage)" }
    $items.Add(@"
            <a class="language-option" href="$(Html $href)" hreflang="$(Html $code)" lang="$(Html $code)" dir="$(Html $locale.dir)">
              <strong>$(Html $locale.nativeLabel)</strong>
              <span>$(Html $label)</span>
            </a>
"@)
  }

  return ($items -join "`n")
}

function New-DocumentLinks {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)]$Documents,
    [Parameter(Mandatory = $true)]$Locales
  )

  $items = New-Object System.Collections.Generic.List[string]
  foreach ($docInfo in $Documents) {
    $actions = New-Object System.Collections.Generic.List[string]
    foreach ($locale in $Locales) {
      $code = [string]$locale.code
      $docPath = Get-DocumentPath -Manifest $Manifest -DocumentId $docInfo.id -LocaleCode $code
      $href = Convert-DocPathToRelativeHref -DocPath $docPath -Context "docs-index"
      $actions.Add("              <a href=`"$(Html $href)`" hreflang=`"$(Html $code)`">$(Html $locale.nativeLabel)</a>")
    }

    $items.Add(@"
          <article class="doc-row">
            <div>
              <h3>$(Html $docInfo.title)</h3>
              <p>$(Html $docInfo.description)</p>
            </div>
            <div class="doc-row-actions">
$($actions -join "`n")
            </div>
          </article>
"@)
  }

  return ($items -join "`n")
}

$manifest = Read-JsonFile -Path $ManifestPath
$locales = @($manifest.locales | Where-Object { $_.enabled -ne $false })
$homeTemplate = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $TemplateRoot "home.html")
$docsTemplate = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $TemplateRoot "docs-index.html")

foreach ($locale in $locales) {
  $localeCode = [string]$locale.code
  $localeJsonPath = Join-Path $LocaleRoot "$localeCode.json"
  if (-not (Test-Path -LiteralPath $localeJsonPath)) {
    throw "Missing website locale file: website/_i18n/$localeCode.json"
  }

  $localeData = Read-JsonFile -Path $localeJsonPath
  $context = if ($localeCode -eq $manifest.defaultLocale) { "root" } else { "locale-home" }
  $assetPrefix = if ($localeCode -eq $manifest.defaultLocale) { "" } else { "../" }
  $outputPath = if ($localeCode -eq $manifest.defaultLocale) {
    Join-Path $WebsiteRoot "index.html"
  } else {
    Join-Path $WebsiteRoot "$localeCode/index.html"
  }

  $homeHrefByLocale = @{}
  foreach ($targetLocale in $locales) {
    $homeHrefByLocale[[string]$targetLocale.code] = Get-LocaleHomePath -Manifest $manifest -LocaleCode ([string]$targetLocale.code)
  }

  $readmePath = Get-DocumentPath -Manifest $manifest -DocumentId "readme" -LocaleCode $localeCode
  $architecturePath = Get-DocumentPath -Manifest $manifest -DocumentId "architecture-overview" -LocaleCode $localeCode
  $docsHref = if ($localeCode -eq $manifest.defaultLocale) {
    "docs/"
  } else {
    Convert-DocPathToRelativeHref -DocPath $readmePath -Context $context
  }
  $architectureHref = Convert-DocPathToRelativeHref -DocPath $architecturePath -Context $context
  $canonicalUrl = Join-SiteUrl -BaseUrl $manifest.siteUrl -Path (Get-LocaleHomePath -Manifest $manifest -LocaleCode $localeCode)

  $homeTokens = @{
    lang = Html $localeCode
    dir = Html $locale.dir
    title = Html $localeData.meta.title
    description = Html $localeData.meta.description
    canonicalUrl = Html $canonicalUrl
    alternateLinks = New-AlternateLinks -Manifest $manifest -Locales $locales -PageKind "home"
    assetPrefix = Html $assetPrefix
    homeUrl = Html "./"
    brandTagline = Html $localeData.brand.tagline
    navLabel = Html $localeData.nav.label
    navLinks = New-NavLinks -LocaleData $localeData -HomeHref "./" -DocsHref $docsHref -ArchitectureHref $architectureHref -FeedbackHref "#feedback"
    languageSwitch = New-LanguageSwitch -Locales $locales -HrefByLocale $homeHrefByLocale -CurrentLocale $localeCode
    heroEyebrow = Html $localeData.home.heroEyebrow
    heroTitle = Html $localeData.home.heroTitle
    heroTagline = Html $localeData.home.heroTagline
    heroLead = Html $localeData.home.heroLead
    actionsLabel = Html $localeData.home.actionsLabel
    heroActions = @"
            <a class="primary-action" href="$(Html $docsHref)">$(Html $localeData.home.primaryAction)</a>
            <a class="secondary-action" href="$(Html $architectureHref)">$(Html $localeData.home.secondaryAction)</a>
            <a class="secondary-action" href="#feedback">$(Html $localeData.nav.feedback)</a>
"@
    architectureEyebrow = Html $localeData.home.architectureEyebrow
    architectureTitle = Html $localeData.home.architectureTitle
    architectureAlt = Html $localeData.home.architectureAlt
    docsEyebrow = Html $localeData.home.docsEyebrow
    docsTitle = Html $localeData.home.docsTitle
    docCards = New-HomeDocCards -Manifest $manifest -Cards $localeData.home.docCards -LocaleCode $localeCode -Context $context
    feedbackEyebrow = Html $localeData.feedback.eyebrow
    feedbackTitle = Html $localeData.feedback.title
    feedbackBody = Html $localeData.feedback.body
    feedbackContactLabel = Html $localeData.feedback.contactLabel
    feedbackCategoryLabel = Html $localeData.feedback.categoryLabel
    feedbackOptions = New-FeedbackOptions -Options $localeData.feedback.categoryOptions
    feedbackContactField = Html $localeData.feedback.contactField
    feedbackContactPlaceholder = Html $localeData.feedback.contactPlaceholder
    feedbackSubjectLabel = Html $localeData.feedback.subjectLabel
    feedbackSubjectPlaceholder = Html $localeData.feedback.subjectPlaceholder
    feedbackMessageLabel = Html $localeData.feedback.messageLabel
    feedbackMessagePlaceholder = Html $localeData.feedback.messagePlaceholder
    feedbackSubmit = Html $localeData.feedback.submit
    feedbackNote = Html $localeData.feedback.note
    footerAuthor = Html $localeData.footer.author
    footerFeedback = Html $localeData.footer.feedback
    footerSubmit = Html $localeData.footer.submit
  }

  Write-Utf8NoBom -Path $outputPath -Content (Apply-Template -Template $homeTemplate -Tokens $homeTokens)
}

$defaultLocaleCode = [string]$manifest.defaultLocale
$defaultLocale = @($locales | Where-Object { $_.code -eq $defaultLocaleCode }) | Select-Object -First 1
$defaultLocaleData = Read-JsonFile -Path (Join-Path $LocaleRoot "$defaultLocaleCode.json")
$docsHrefByLocale = @{}
foreach ($targetLocale in $locales) {
  $targetCode = [string]$targetLocale.code
  $docsHrefByLocale[$targetCode] = if ($targetCode -eq $manifest.defaultLocale) {
    "/docs/"
  } else {
    "/$(Get-DocumentPath -Manifest $manifest -DocumentId "readme" -LocaleCode $targetCode)"
  }
}

$docsIndexTokens = @{
  lang = Html $defaultLocaleCode
  dir = Html $defaultLocale.dir
  title = Html $defaultLocaleData.docsIndex.metaTitle
  description = Html $defaultLocaleData.docsIndex.metaDescription
  canonicalUrl = Html (Join-SiteUrl -BaseUrl $manifest.siteUrl -Path "/docs/")
  alternateLinks = New-AlternateLinks -Manifest $manifest -Locales $locales -PageKind "docs-index"
  assetPrefix = "../"
  homeUrl = "../"
  brandTagline = Html $defaultLocaleData.brand.tagline
  navLabel = Html $defaultLocaleData.nav.label
  navLinks = New-NavLinks -LocaleData $defaultLocaleData -HomeHref "../" -DocsHref "./" -ArchitectureHref "en/architecture-overview.md" -FeedbackHref "../#feedback"
  languageSwitch = New-LanguageSwitch -Locales $locales -HrefByLocale $docsHrefByLocale -CurrentLocale $defaultLocaleCode
  heroEyebrow = Html $defaultLocaleData.docsIndex.heroEyebrow
  heroTitle = Html $defaultLocaleData.docsIndex.heroTitle
  heroLead = Html $defaultLocaleData.docsIndex.heroLead
  actionsLabel = Html $defaultLocaleData.docsIndex.actionsLabel
  heroActions = @"
            <a class="primary-action" href="en/architecture-overview.md">$(Html $defaultLocaleData.docsIndex.primaryAction)</a>
            <a class="secondary-action" href="https://github.com/Yetmos/Eva-CLI">$(Html $defaultLocaleData.docsIndex.secondaryAction)</a>
"@
  languagePanelLabel = Html $defaultLocaleData.docsIndex.languagePanelLabel
  languagePanelKicker = Html $defaultLocaleData.docsIndex.languagePanelKicker
  languagePanelTitle = Html $defaultLocaleData.docsIndex.languagePanelTitle
  languageList = New-LanguageList -Manifest $manifest -Locales $locales
  fallbackNote = Html $defaultLocaleData.docsIndex.fallbackNote
  documentsEyebrow = Html $defaultLocaleData.docsIndex.documentsEyebrow
  documentsTitle = Html $defaultLocaleData.docsIndex.documentsTitle
  documentLinks = New-DocumentLinks -Manifest $manifest -Documents $defaultLocaleData.docsIndex.documents -Locales $locales
  footerAuthor = Html $defaultLocaleData.footer.author
  footerFeedback = Html $defaultLocaleData.footer.feedback
  footerSubmit = Html $defaultLocaleData.footer.submit
  feedbackUrl = "../#feedback"
}

Write-Utf8NoBom -Path (Join-Path $WebsiteRoot "docs/index.html") -Content (Apply-Template -Template $docsTemplate -Tokens $docsIndexTokens)

Write-Host "Generated localized website pages for $($locales.Count) locale(s)."
