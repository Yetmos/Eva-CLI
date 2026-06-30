#!/usr/bin/env pwsh
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$ManifestPath = Join-Path $Root "docs/_i18n/manifest.json"
$LocaleRoot = Join-Path $Root "website/_i18n"
$WebsiteRoot = Join-Path $Root "website"
$DocsRoot = Join-Path $Root "docs"
$BlogDataPath = Join-Path $Root "website/_blog/posts.json"

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

function Get-BlogIndexSitePath {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$LocaleCode
  )

  if ($LocaleCode -eq $Manifest.defaultLocale) {
    return "/blog/"
  }

  return "/$LocaleCode/blog/"
}

function Get-BlogCategorySitePath {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$LocaleCode,
    [Parameter(Mandatory = $true)][string]$CategoryId
  )

  return "$(Get-BlogIndexSitePath -Manifest $Manifest -LocaleCode $LocaleCode)category/$CategoryId/"
}

function Get-BlogPostSitePath {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$LocaleCode,
    [Parameter(Mandatory = $true)][string]$Slug
  )

  return "$(Get-BlogIndexSitePath -Manifest $Manifest -LocaleCode $LocaleCode)$Slug/"
}

function Get-BlogIndexOutputPath {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$LocaleCode
  )

  if ($LocaleCode -eq $Manifest.defaultLocale) {
    return Join-Path $WebsiteRoot "blog/index.html"
  }

  return Join-Path $WebsiteRoot "$LocaleCode/blog/index.html"
}

function Get-BlogCategoryOutputPath {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$LocaleCode,
    [Parameter(Mandatory = $true)][string]$CategoryId
  )

  if ($LocaleCode -eq $Manifest.defaultLocale) {
    return Join-Path $WebsiteRoot "blog/category/$CategoryId/index.html"
  }

  return Join-Path $WebsiteRoot "$LocaleCode/blog/category/$CategoryId/index.html"
}

function Get-BlogPostOutputPath {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$LocaleCode,
    [Parameter(Mandatory = $true)][string]$Slug
  )

  if ($LocaleCode -eq $Manifest.defaultLocale) {
    return Join-Path $WebsiteRoot "blog/$Slug/index.html"
  }

  return Join-Path $WebsiteRoot "$LocaleCode/blog/$Slug/index.html"
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

function Assert-NoTemplateToken {
  param(
    [Parameter(Mandatory = $true)][string]$Html,
    [Parameter(Mandatory = $true)][string]$Path
  )

  if ($Html -match "{{[^}]+}}") {
    Fail "Generated HTML contains unresolved token '$($Matches[0])': $Path"
  }
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
  foreach ($path in @("meta", "brand", "nav", "home", "docsIndex", "blog", "feedback", "footer")) {
    if ($null -eq (Get-PropertyValue -Object $localeData -Name $path)) {
      Fail "website/_i18n/$($locale.code).json missing '$path'."
    }
  }

  if ([string]::IsNullOrWhiteSpace($localeData.nav.blog)) {
    Fail "website/_i18n/$($locale.code).json missing 'nav.blog'."
  }

  foreach ($field in @("progressAction", "progressEyebrow", "progressTitle", "progressCurrentLabel", "progressCurrentKicker", "progressCurrentTitle", "progressCurrentBody", "progressTimelineLabel", "progressRoadmapLabel", "progressNextTitle")) {
    if ([string]::IsNullOrWhiteSpace($localeData.home.$field)) {
      Fail "website/_i18n/$($locale.code).json missing 'home.$field'."
    }
  }

  if ($null -eq $localeData.home.progressStages -or @($localeData.home.progressStages).Count -eq 0) {
    Fail "website/_i18n/$($locale.code).json missing 'home.progressStages'."
  }

  if ($null -eq $localeData.home.progressNextItems -or @($localeData.home.progressNextItems).Count -eq 0) {
    Fail "website/_i18n/$($locale.code).json missing 'home.progressNextItems'."
  }

  foreach ($field in @("metaTitle", "metaDescription", "heroEyebrow", "heroTitle", "heroLead", "categoriesTitle", "allPostsTitle", "categoryLabel", "dateLabel", "readMore", "backToBlog", "emptyState")) {
    if ([string]::IsNullOrWhiteSpace($localeData.blog.$field)) {
      Fail "website/_i18n/$($locale.code).json missing 'blog.$field'."
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
  Assert-NoTemplateToken -Html $homeHtml -Path $homePath
  if ($homeHtml -notmatch "<html lang=`"$([regex]::Escape($locale.code))`" dir=`"$([regex]::Escape($locale.dir))`"") {
    Fail "Generated home page for '$($locale.code)' has incorrect html lang/dir."
  }
  if ($homeHtml -notmatch 'rel="canonical"') {
    Fail "Generated home page for '$($locale.code)' is missing canonical link."
  }
  if ($homeHtml -notmatch 'hreflang="x-default"') {
    Fail "Generated home page for '$($locale.code)' is missing x-default hreflang."
  }
  if ($homeHtml -match 'id="discussion"|href="#discussion"|https://giscus\.app/client\.js|data-eva-chat|chat/chat-app\.js|data-chat-action=') {
    Fail "Generated home page for '$($locale.code)' still contains discussion or chat markup."
  }
  if ($homeHtml -notmatch 'id="development-progress"') {
    Fail "Generated home page for '$($locale.code)' is missing the development progress section."
  }
  if ($homeHtml -notmatch 'href="#development-progress"') {
    Fail "Generated home page for '$($locale.code)' is missing the development progress navigation link."
  }
  if ($homeHtml -notmatch 'progress-stage-current') {
    Fail "Generated home page for '$($locale.code)' is missing the current progress stage."
  }
  if ($homeHtml -notmatch 'progress-roadmap-link') {
    Fail "Generated home page for '$($locale.code)' is missing the zero-to-one roadmap link."
  }
  if ($homeHtml -notmatch 'blog/') {
    Fail "Generated home page for '$($locale.code)' is missing the blog navigation link."
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

if ($null -eq (Get-PropertyValue -Object $manifest -Name "assets")) {
  Fail "manifest.assets is required for localized content assets."
}

foreach ($asset in $manifest.assets) {
  if ([string]::IsNullOrWhiteSpace($asset.id)) {
    Fail "An asset entry is missing id."
  }
  if ([string]::IsNullOrWhiteSpace($asset.source)) {
    Fail "Asset '$($asset.id)' is missing source."
  }
  if (-not (Test-RepoPath -RelativePath $asset.source)) {
    Fail "Asset '$($asset.id)' source does not exist: $($asset.source)"
  }

  foreach ($locale in $locales) {
    if ($locale.code -eq $manifest.defaultLocale) {
      continue
    }

    $status = Get-PropertyValue -Object $asset.status -Name $locale.code
    if ([string]::IsNullOrWhiteSpace($status)) {
      Fail "Asset '$($asset.id)' is missing translation status for '$($locale.code)'."
    }

    if ($status -notin @("current", "needs-review", "stale", "missing", "partial")) {
      Fail "Asset '$($asset.id)' has invalid status '$status' for '$($locale.code)'."
    }

    $translation = Get-PropertyValue -Object $asset.translations -Name $locale.code
    if ($status -ne "missing") {
      if ([string]::IsNullOrWhiteSpace($translation)) {
        Fail "Asset '$($asset.id)' has status '$status' but no translation path for '$($locale.code)'."
      }
      if (-not (Test-RepoPath -RelativePath $translation)) {
        Fail "Asset '$($asset.id)' translation does not exist for '$($locale.code)': $translation"
      }
    }
  }
}

$architectureAsset = @($manifest.assets | Where-Object { $_.id -eq "architecture-diagram" }) | Select-Object -First 1
if ($null -eq $architectureAsset) {
  Fail "Missing asset mapping for 'architecture-diagram'."
}

foreach ($locale in $locales) {
  $homePath = if ($locale.code -eq $manifest.defaultLocale) {
    Join-Path $WebsiteRoot "index.html"
  } else {
    Join-Path $WebsiteRoot "$($locale.code)/index.html"
  }
  $homeHtml = Get-Content -Raw -Encoding UTF8 -LiteralPath $homePath
  $expectedAssetPath = if ($locale.code -eq $manifest.defaultLocale) {
    $architectureAsset.source
  } else {
    $localizedAssetPath = Get-PropertyValue -Object $architectureAsset.translations -Name $locale.code
    if ([string]::IsNullOrWhiteSpace($localizedAssetPath)) {
      $architectureAsset.source
    } else {
      $localizedAssetPath
    }
  }
  $expectedHref = if ($locale.code -eq $manifest.defaultLocale) {
    $expectedAssetPath
  } else {
    "../$expectedAssetPath"
  }

  if ($homeHtml -notmatch [regex]::Escape($expectedHref)) {
    Fail "Generated home page for '$($locale.code)' does not reference localized architecture asset '$expectedHref'."
  }
}

$docsIndexPath = Join-Path $WebsiteRoot "docs/index.html"
if (-not (Test-Path -LiteralPath $docsIndexPath)) {
  Fail "Missing generated website/docs/index.html."
}

$docsIndexHtml = Get-Content -Raw -Encoding UTF8 -LiteralPath $docsIndexPath
Assert-NoTemplateToken -Html $docsIndexHtml -Path $docsIndexPath
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

if (-not (Test-Path -LiteralPath $BlogDataPath)) {
  Fail "Missing website/_blog/posts.json."
}

$blogData = Read-JsonFile -Path $BlogDataPath
$blogCategories = @($blogData.categories)
$blogPosts = @($blogData.posts)
if ($blogCategories.Count -eq 0) {
  Fail "website/_blog/posts.json must define at least one category."
}
if ($blogPosts.Count -eq 0) {
  Fail "website/_blog/posts.json must define at least one post."
}

$localeCodes = @($locales | ForEach-Object { [string]$_.code })
$categoryIds = New-Object System.Collections.Generic.HashSet[string]
foreach ($category in $blogCategories) {
  if ([string]::IsNullOrWhiteSpace($category.id)) {
    Fail "A blog category is missing id."
  }
  if (-not $categoryIds.Add([string]$category.id)) {
    Fail "Duplicate blog category id '$($category.id)'."
  }
}

$slugsByLocale = @{}
foreach ($locale in $locales) {
  $slugsByLocale[[string]$locale.code] = New-Object System.Collections.Generic.HashSet[string]
}

foreach ($post in $blogPosts) {
  foreach ($field in @("id", "locale", "slug", "title", "description", "date", "category", "contentPath")) {
    $value = Get-PropertyValue -Object $post -Name $field
    if ([string]::IsNullOrWhiteSpace([string]$value)) {
      Fail "A blog post is missing '$field'."
    }
  }

  if ([string]$post.locale -notin $localeCodes) {
    Fail "Blog post '$($post.id)' uses unknown or disabled locale '$($post.locale)'."
  }
  if (-not $categoryIds.Contains([string]$post.category)) {
    Fail "Blog post '$($post.id)' references unknown category '$($post.category)'."
  }
  if (-not $slugsByLocale[[string]$post.locale].Add([string]$post.slug)) {
    Fail "Duplicate blog slug '$($post.slug)' for locale '$($post.locale)'."
  }
  if (-not (Test-RepoPath -RelativePath ([string]$post.contentPath))) {
    Fail "Blog post '$($post.id)' content file does not exist: $($post.contentPath)"
  }
  try {
    [void][datetime]::Parse([string]$post.date)
  } catch {
    Fail "Blog post '$($post.id)' has invalid date '$($post.date)'."
  }
}

foreach ($locale in $locales) {
  $localeCode = [string]$locale.code
  $blogIndexPath = Get-BlogIndexOutputPath -Manifest $manifest -LocaleCode $localeCode
  if (-not (Test-Path -LiteralPath $blogIndexPath)) {
    Fail "Missing generated blog index for locale '$localeCode': $blogIndexPath"
  }

  $blogIndexHtml = Get-Content -Raw -Encoding UTF8 -LiteralPath $blogIndexPath
  Assert-NoTemplateToken -Html $blogIndexHtml -Path $blogIndexPath
  $expectedIndexCanonical = Join-SiteUrl -BaseUrl $manifest.siteUrl -Path (Get-BlogIndexSitePath -Manifest $manifest -LocaleCode $localeCode)
  if ($blogIndexHtml -notmatch [regex]::Escape("rel=`"canonical`" href=`"$expectedIndexCanonical`"")) {
    Fail "Generated blog index for '$localeCode' is missing expected canonical URL."
  }
  if ($blogIndexHtml -notmatch 'hreflang="x-default"') {
    Fail "Generated blog index for '$localeCode' is missing x-default hreflang."
  }

  foreach ($category in $blogCategories) {
    $categoryId = [string]$category.id
    $categoryPath = Get-BlogCategoryOutputPath -Manifest $manifest -LocaleCode $localeCode -CategoryId $categoryId
    if (-not (Test-Path -LiteralPath $categoryPath)) {
      Fail "Missing generated blog category page for locale '$localeCode' and category '$categoryId': $categoryPath"
    }

    $categoryHtml = Get-Content -Raw -Encoding UTF8 -LiteralPath $categoryPath
    Assert-NoTemplateToken -Html $categoryHtml -Path $categoryPath
    $expectedCategoryCanonical = Join-SiteUrl -BaseUrl $manifest.siteUrl -Path (Get-BlogCategorySitePath -Manifest $manifest -LocaleCode $localeCode -CategoryId $categoryId)
    if ($categoryHtml -notmatch [regex]::Escape("rel=`"canonical`" href=`"$expectedCategoryCanonical`"")) {
      Fail "Generated blog category page for '$localeCode/$categoryId' is missing expected canonical URL."
    }
  }

  $localePosts = @($blogPosts | Where-Object { $_.locale -eq $localeCode })
  foreach ($post in $localePosts) {
    $postPath = Get-BlogPostOutputPath -Manifest $manifest -LocaleCode $localeCode -Slug ([string]$post.slug)
    if (-not (Test-Path -LiteralPath $postPath)) {
      Fail "Missing generated blog post page for '$localeCode/$($post.slug)': $postPath"
    }

    $postHtml = Get-Content -Raw -Encoding UTF8 -LiteralPath $postPath
    Assert-NoTemplateToken -Html $postHtml -Path $postPath
    $expectedPostCanonical = Join-SiteUrl -BaseUrl $manifest.siteUrl -Path (Get-BlogPostSitePath -Manifest $manifest -LocaleCode $localeCode -Slug ([string]$post.slug))
    if ($postHtml -notmatch [regex]::Escape("rel=`"canonical`" href=`"$expectedPostCanonical`"")) {
      Fail "Generated blog post '$localeCode/$($post.slug)' is missing expected canonical URL."
    }

    $localizedSiblings = @($blogPosts | Where-Object { $_.id -eq $post.id })
    foreach ($sibling in $localizedSiblings) {
      $siblingHref = Join-SiteUrl -BaseUrl $manifest.siteUrl -Path (Get-BlogPostSitePath -Manifest $manifest -LocaleCode ([string]$sibling.locale) -Slug ([string]$sibling.slug))
      if ($postHtml -notmatch [regex]::Escape("hreflang=`"$($sibling.locale)`" href=`"$siblingHref`"")) {
        Fail "Generated blog post '$localeCode/$($post.slug)' is missing hreflang for '$($sibling.locale)'."
      }
    }
  }
}

Write-Host "i18n and blog structure validated for $($locales.Count) locale(s), $($manifest.documents.Count) document(s), and $($blogPosts.Count) blog post(s)."
