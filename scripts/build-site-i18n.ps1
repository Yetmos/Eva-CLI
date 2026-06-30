#!/usr/bin/env pwsh
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$ManifestPath = Join-Path $Root "docs/_i18n/manifest.json"
$TemplateRoot = Join-Path $Root "website/_templates"
$LocaleRoot = Join-Path $Root "website/_i18n"
$WebsiteRoot = Join-Path $Root "website"
$BlogDataPath = Join-Path $Root "website/_blog/posts.json"

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

function Get-Asset {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$Id
  )

  $asset = @($Manifest.assets | Where-Object { $_.id -eq $Id }) | Select-Object -First 1
  if ($null -eq $asset) {
    throw "Asset '$Id' is not registered in docs/_i18n/manifest.json."
  }

  return $asset
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

function Get-AssetPath {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$AssetId,
    [Parameter(Mandatory = $true)][string]$LocaleCode
  )

  $asset = Get-Asset -Manifest $Manifest -Id $AssetId
  if ($LocaleCode -eq $Manifest.defaultLocale) {
    return $asset.source
  }

  $translation = Get-PropertyValue -Object $asset.translations -Name $LocaleCode
  if ([string]::IsNullOrWhiteSpace($translation)) {
    return $asset.source
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

function Convert-SitePathToRelativeHref {
  param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][ValidateSet("root", "locale-home", "docs-index")][string]$Context
  )

  switch ($Context) {
    "root" { return $Path }
    "locale-home" { return "../$Path" }
    "docs-index" { return "../$Path" }
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

function Get-BlogRelativeHref {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$FromLocale,
    [Parameter(Mandatory = $true)][ValidateSet("blog-index", "blog-category", "blog-post")][string]$FromKind,
    [Parameter(Mandatory = $true)][string]$TargetSitePath
  )

  if ($FromLocale -eq $Manifest.defaultLocale) {
    switch ($FromKind) {
      "blog-index" {
        if ($TargetSitePath -eq "/") {
          return "../"
        }
        return "../$($TargetSitePath.TrimStart("/"))"
      }
      "blog-category" {
        if ($TargetSitePath -eq "/") {
          return "../../../"
        }
        return "../../../$($TargetSitePath.TrimStart("/"))"
      }
      "blog-post" {
        if ($TargetSitePath -eq "/") {
          return "../../"
        }
        return "../../$($TargetSitePath.TrimStart("/"))"
      }
    }
  }

  switch ($FromKind) {
    "blog-index" {
      if ($TargetSitePath -eq "/") {
        return "../../"
      }
      return "../../$($TargetSitePath.TrimStart("/"))"
    }
    "blog-category" {
      if ($TargetSitePath -eq "/") {
        return "../../../../"
      }
      return "../../../../$($TargetSitePath.TrimStart("/"))"
    }
    "blog-post" {
      if ($TargetSitePath -eq "/") {
        return "../../../"
      }
      return "../../../$($TargetSitePath.TrimStart("/"))"
    }
  }
}

function Get-BlogAssetPrefix {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$LocaleCode,
    [Parameter(Mandatory = $true)][ValidateSet("blog-index", "blog-category", "blog-post")][string]$PageKind
  )

  $rootPath = Get-BlogRelativeHref -Manifest $Manifest -FromLocale $LocaleCode -FromKind $PageKind -TargetSitePath "/"
  return $rootPath
}

function Get-CategoryLabel {
  param(
    [Parameter(Mandatory = $true)]$Category,
    [Parameter(Mandatory = $true)][string]$LocaleCode
  )

  $label = Get-PropertyValue -Object $Category.labels -Name $LocaleCode
  if ([string]::IsNullOrWhiteSpace($label)) {
    return [string]$Category.id
  }

  return [string]$label
}

function New-BlogAlternateLinks {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)]$Locales,
    [Parameter(Mandatory = $true)][ValidateSet("index", "category", "post")][string]$PageKind,
    [string]$CategoryId,
    $Post,
    [Parameter(Mandatory = $true)]$Posts
  )

  $links = New-Object System.Collections.Generic.List[string]
  foreach ($locale in $Locales) {
    $code = [string]$locale.code
    if ($PageKind -eq "index") {
      $path = Get-BlogIndexSitePath -Manifest $Manifest -LocaleCode $code
    } elseif ($PageKind -eq "category") {
      $path = Get-BlogCategorySitePath -Manifest $Manifest -LocaleCode $code -CategoryId $CategoryId
    } else {
      $localizedPost = @($Posts | Where-Object { $_.id -eq $Post.id -and $_.locale -eq $code }) | Select-Object -First 1
      if ($null -eq $localizedPost) {
        continue
      }
      $path = Get-BlogPostSitePath -Manifest $Manifest -LocaleCode $code -Slug ([string]$localizedPost.slug)
    }

    $href = Html (Join-SiteUrl -BaseUrl $Manifest.siteUrl -Path $path)
    $links.Add("    <link rel=`"alternate`" hreflang=`"$(Html $code)`" href=`"$href`">")
  }

  $defaultPath = if ($PageKind -eq "index") {
    Get-BlogIndexSitePath -Manifest $Manifest -LocaleCode ([string]$Manifest.defaultLocale)
  } elseif ($PageKind -eq "category") {
    Get-BlogCategorySitePath -Manifest $Manifest -LocaleCode ([string]$Manifest.defaultLocale) -CategoryId $CategoryId
  } else {
    $defaultPost = @($Posts | Where-Object { $_.id -eq $Post.id -and $_.locale -eq $Manifest.defaultLocale }) | Select-Object -First 1
    if ($null -eq $defaultPost) {
      Get-BlogPostSitePath -Manifest $Manifest -LocaleCode ([string]$Post.locale) -Slug ([string]$Post.slug)
    } else {
      Get-BlogPostSitePath -Manifest $Manifest -LocaleCode ([string]$Manifest.defaultLocale) -Slug ([string]$defaultPost.slug)
    }
  }
  $links.Add("    <link rel=`"alternate`" hreflang=`"x-default`" href=`"$(Html (Join-SiteUrl -BaseUrl $Manifest.siteUrl -Path $defaultPath))`">")

  return ($links -join "`n")
}

function New-BlogLanguageSwitch {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)]$Locales,
    [Parameter(Mandatory = $true)][string]$CurrentLocale,
    [Parameter(Mandatory = $true)][ValidateSet("blog-index", "blog-category", "blog-post")][string]$FromKind,
    [Parameter(Mandatory = $true)][ValidateSet("index", "category", "post")][string]$TargetKind,
    [string]$CategoryId,
    $Post,
    [Parameter(Mandatory = $true)]$Posts
  )

  $hrefByLocale = @{}
  foreach ($locale in $Locales) {
    $code = [string]$locale.code
    if ($TargetKind -eq "index") {
      $targetPath = Get-BlogIndexSitePath -Manifest $Manifest -LocaleCode $code
    } elseif ($TargetKind -eq "category") {
      $targetPath = Get-BlogCategorySitePath -Manifest $Manifest -LocaleCode $code -CategoryId $CategoryId
    } else {
      $localizedPost = @($Posts | Where-Object { $_.id -eq $Post.id -and $_.locale -eq $code }) | Select-Object -First 1
      if ($null -eq $localizedPost) {
        $targetPath = Get-BlogIndexSitePath -Manifest $Manifest -LocaleCode $code
      } else {
        $targetPath = Get-BlogPostSitePath -Manifest $Manifest -LocaleCode $code -Slug ([string]$localizedPost.slug)
      }
    }

    $hrefByLocale[$code] = Get-BlogRelativeHref -Manifest $Manifest -FromLocale $CurrentLocale -FromKind $FromKind -TargetSitePath $targetPath
  }

  return New-LanguageSwitch -Locales $Locales -HrefByLocale $hrefByLocale -CurrentLocale $CurrentLocale
}

function New-BlogCategoryLinks {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)]$Categories,
    [Parameter(Mandatory = $true)]$Posts,
    [Parameter(Mandatory = $true)][string]$LocaleCode,
    [Parameter(Mandatory = $true)][ValidateSet("blog-index", "blog-category", "blog-post")][string]$FromKind
  )

  $items = New-Object System.Collections.Generic.List[string]
  foreach ($category in $Categories) {
    $categoryId = [string]$category.id
    $label = Get-CategoryLabel -Category $category -LocaleCode $LocaleCode
    $count = @($Posts | Where-Object { $_.locale -eq $LocaleCode -and $_.category -eq $categoryId }).Count
    $targetPath = Get-BlogCategorySitePath -Manifest $Manifest -LocaleCode $LocaleCode -CategoryId $categoryId
    $href = Get-BlogRelativeHref -Manifest $Manifest -FromLocale $LocaleCode -FromKind $FromKind -TargetSitePath $targetPath
    $items.Add("            <a href=`"$(Html $href)`">$(Html $label)<span>$(Html ([string]$count))</span></a>")
  }

  return ($items -join "`n")
}

function New-BlogPostCards {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)]$Posts,
    [Parameter(Mandatory = $true)]$Categories,
    [Parameter(Mandatory = $true)]$LocaleData,
    [Parameter(Mandatory = $true)][string]$LocaleCode,
    [Parameter(Mandatory = $true)][ValidateSet("blog-index", "blog-category", "blog-post")][string]$FromKind
  )

  $items = New-Object System.Collections.Generic.List[string]
  $sortedPosts = @($Posts | Sort-Object -Property @{ Expression = { [datetime]$_.date }; Descending = $true }, @{ Expression = { [string]$_.title }; Ascending = $true })
  foreach ($post in $sortedPosts) {
    $category = @($Categories | Where-Object { $_.id -eq $post.category }) | Select-Object -First 1
    $categoryLabel = Get-CategoryLabel -Category $category -LocaleCode $LocaleCode
    $postPath = Get-BlogPostSitePath -Manifest $Manifest -LocaleCode $LocaleCode -Slug ([string]$post.slug)
    $postHref = Get-BlogRelativeHref -Manifest $Manifest -FromLocale $LocaleCode -FromKind $FromKind -TargetSitePath $postPath
    $categoryPath = Get-BlogCategorySitePath -Manifest $Manifest -LocaleCode $LocaleCode -CategoryId ([string]$post.category)
    $categoryHref = Get-BlogRelativeHref -Manifest $Manifest -FromLocale $LocaleCode -FromKind $FromKind -TargetSitePath $categoryPath
    $items.Add(@"
            <article class="blog-card">
              <div class="blog-meta">
                <span>$(Html $LocaleData.blog.dateLabel) <time datetime="$(Html $post.date)">$(Html $post.date)</time></span>
                <a href="$(Html $categoryHref)">$(Html $LocaleData.blog.categoryLabel) $(Html $categoryLabel)</a>
              </div>
              <h3><a href="$(Html $postHref)">$(Html $post.title)</a></h3>
              <p>$(Html $post.description)</p>
              <a class="blog-card-link" href="$(Html $postHref)">$(Html $LocaleData.blog.readMore)</a>
            </article>
"@)
  }

  if ($items.Count -eq 0) {
    return "            <p class=`"blog-empty`">$(Html $LocaleData.blog.emptyState)</p>"
  }

  return ($items -join "`n")
}

function New-FeaturedBlogCards {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)]$Posts,
    [Parameter(Mandatory = $true)]$Categories,
    [Parameter(Mandatory = $true)]$LocaleData,
    [Parameter(Mandatory = $true)][string]$LocaleCode,
    [Parameter(Mandatory = $true)][ValidateSet("root", "locale-home")][string]$Context,
    [int]$Limit = 3
  )

  $items = New-Object System.Collections.Generic.List[string]
  $sortedPosts = @($Posts |
    Sort-Object -Property @{ Expression = { [datetime]$_.date }; Descending = $true }, @{ Expression = { [string]$_.title }; Ascending = $true } |
    Select-Object -First $Limit)

  foreach ($post in $sortedPosts) {
    $category = @($Categories | Where-Object { $_.id -eq $post.category }) | Select-Object -First 1
    $categoryLabel = Get-CategoryLabel -Category $category -LocaleCode $LocaleCode
    $postPath = Get-BlogPostSitePath -Manifest $Manifest -LocaleCode $LocaleCode -Slug ([string]$post.slug)
    $categoryPath = Get-BlogCategorySitePath -Manifest $Manifest -LocaleCode $LocaleCode -CategoryId ([string]$post.category)
    if ($Context -eq "locale-home") {
      $localePrefix = "/$LocaleCode/"
      $postHref = if ($postPath.StartsWith($localePrefix)) { $postPath.Substring($localePrefix.Length) } else { Convert-SitePathToRelativeHref -Path ($postPath.TrimStart("/")) -Context $Context }
      $categoryHref = if ($categoryPath.StartsWith($localePrefix)) { $categoryPath.Substring($localePrefix.Length) } else { Convert-SitePathToRelativeHref -Path ($categoryPath.TrimStart("/")) -Context $Context }
    } else {
      $postHref = $postPath.TrimStart("/")
      $categoryHref = $categoryPath.TrimStart("/")
    }
    $items.Add(@"
          <article class="blog-card home-blog-card">
            <div class="blog-meta">
              <span>$(Html $LocaleData.blog.dateLabel) <time datetime="$(Html $post.date)">$(Html $post.date)</time></span>
              <a href="$(Html $categoryHref)">$(Html $LocaleData.blog.categoryLabel) $(Html $categoryLabel)</a>
            </div>
            <h3><a href="$(Html $postHref)">$(Html $post.title)</a></h3>
            <p>$(Html $post.description)</p>
            <a class="blog-card-link" href="$(Html $postHref)">$(Html $LocaleData.blog.readMore)</a>
          </article>
"@)
  }

  if ($items.Count -eq 0) {
    return "          <p class=`"blog-empty`">$(Html $LocaleData.blog.emptyState)</p>"
  }

  return ($items -join "`n")
}

function Assert-BlogData {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)]$Locales,
    [Parameter(Mandatory = $true)]$Categories,
    [Parameter(Mandatory = $true)]$Posts
  )

  $localeCodes = @($Locales | ForEach-Object { [string]$_.code })
  if ($Categories.Count -eq 0) {
    throw "Blog metadata must contain at least one category."
  }

  $categoryIds = New-Object System.Collections.Generic.HashSet[string]
  foreach ($category in $Categories) {
    $categoryId = Get-PropertyValue -Object $category -Name "id"
    if ([string]::IsNullOrWhiteSpace($categoryId)) {
      throw "Blog category is missing id."
    }
    if (-not $categoryIds.Add([string]$categoryId)) {
      throw "Duplicate blog category id: $categoryId"
    }
  }

  $slugsByLocale = @{}
  foreach ($localeCode in $localeCodes) {
    $slugsByLocale[$localeCode] = New-Object System.Collections.Generic.HashSet[string]
  }

  foreach ($post in $Posts) {
    foreach ($field in @("id", "locale", "slug", "title", "description", "date", "category", "contentPath")) {
      $value = Get-PropertyValue -Object $post -Name $field
      if ([string]::IsNullOrWhiteSpace([string]$value)) {
        throw "Blog post is missing required field '$field'."
      }
    }

    $localeCode = [string]$post.locale
    if ($localeCode -notin $localeCodes) {
      throw "Blog post '$($post.id)' uses disabled or unknown locale '$localeCode'."
    }

    if (-not $categoryIds.Contains([string]$post.category)) {
      throw "Blog post '$($post.id)' references unknown category '$($post.category)'."
    }

    if (-not $slugsByLocale[$localeCode].Add([string]$post.slug)) {
      throw "Duplicate blog slug '$($post.slug)' for locale '$localeCode'."
    }

    $contentPath = Join-Path $Root (([string]$post.contentPath) -replace "/", [System.IO.Path]::DirectorySeparatorChar)
    if (-not (Test-Path -LiteralPath $contentPath)) {
      throw "Blog post '$($post.id)' content file does not exist: $($post.contentPath)"
    }

    try {
      [void][datetime]::Parse([string]$post.date)
    } catch {
      throw "Blog post '$($post.id)' has invalid date '$($post.date)'."
    }
  }
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
    [Parameter(Mandatory = $true)][string]$BlogHref,
    [Parameter(Mandatory = $true)][string]$ArchitectureHref,
    [Parameter(Mandatory = $true)][string]$FeedbackHref
  )

  return @"
        <a href="$(Html $HomeHref)">$(Html $LocaleData.nav.home)</a>
        <a href="$(Html $DocsHref)">$(Html $LocaleData.nav.docs)</a>
        <a href="$(Html $BlogHref)">$(Html $LocaleData.nav.blog)</a>
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

function New-HeroHighlights {
  param([Parameter(Mandatory = $true)]$Highlights)

  $items = New-Object System.Collections.Generic.List[string]
  foreach ($highlight in $Highlights) {
    $items.Add("            <span>$(Html $highlight)</span>")
  }

  return ($items -join "`n")
}

function New-RuntimeSteps {
  param([Parameter(Mandatory = $true)]$Steps)

  $items = New-Object System.Collections.Generic.List[string]
  foreach ($step in $Steps) {
    $items.Add(@"
            <li>
              <b>$(Html $step.label)</b>
              <span>$(Html $step.description)</span>
            </li>
"@)
  }

  return ($items -join "`n")
}

function New-ProgressStages {
  param([Parameter(Mandatory = $true)]$Stages)

  $allowedStatuses = @("complete", "current", "next", "later")
  $items = New-Object System.Collections.Generic.List[string]
  foreach ($stage in $Stages) {
    $status = [string]$stage.status
    if ($status -notin $allowedStatuses) {
      throw "Unsupported home.progressStages status '$status'."
    }

    $items.Add(@"
            <li class="progress-stage progress-stage-$status">
              <span class="progress-marker">$(Html $stage.badge)</span>
              <div>
                <strong>$(Html $stage.label)</strong>
                <p>$(Html $stage.description)</p>
              </div>
            </li>
"@)
  }

  return ($items -join "`n")
}

function New-ProgressNextItems {
  param([Parameter(Mandatory = $true)]$Items)

  $output = New-Object System.Collections.Generic.List[string]
  foreach ($item in $Items) {
    $output.Add("            <li>$(Html $item)</li>")
  }

  return ($output -join "`n")
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
    $label = if ($code -eq $Manifest.defaultLocale) {
      "Default entry"
    } elseif ($locale.coverage -eq "detailed-source") {
      "Detailed source"
    } else {
      "Translation: $($locale.coverage)"
    }
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
$blogIndexTemplate = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $TemplateRoot "blog-index.html")
$blogCategoryTemplate = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $TemplateRoot "blog-category.html")
$blogPostTemplate = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $TemplateRoot "blog-post.html")
$blogData = Read-JsonFile -Path $BlogDataPath
$blogCategories = @($blogData.categories)
$blogPosts = @($blogData.posts)
Assert-BlogData -Manifest $manifest -Locales $locales -Categories $blogCategories -Posts $blogPosts

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
  $progressRoadmapPath = Get-DocumentPath -Manifest $manifest -DocumentId "zero-to-one-roadmap" -LocaleCode $localeCode
  $architectureImagePath = Get-AssetPath -Manifest $manifest -AssetId "architecture-diagram" -LocaleCode $localeCode
  $architectureImageHref = Convert-SitePathToRelativeHref -Path $architectureImagePath -Context $context
  $docsHref = if ($localeCode -eq $manifest.defaultLocale) {
    "docs/"
  } else {
    Convert-DocPathToRelativeHref -DocPath $readmePath -Context $context
  }
  $architectureHref = Convert-DocPathToRelativeHref -DocPath $architecturePath -Context $context
  $progressRoadmapHref = Convert-DocPathToRelativeHref -DocPath $progressRoadmapPath -Context $context
  $blogHref = "blog/"
  $canonicalUrl = Join-SiteUrl -BaseUrl $manifest.siteUrl -Path (Get-LocaleHomePath -Manifest $manifest -LocaleCode $localeCode)
  $localePosts = @($blogPosts | Where-Object { $_.locale -eq $localeCode })

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
    navLinks = New-NavLinks -LocaleData $localeData -HomeHref "./" -DocsHref $docsHref -BlogHref $blogHref -ArchitectureHref $architectureHref -FeedbackHref "#feedback"
    languageSwitch = New-LanguageSwitch -Locales $locales -HrefByLocale $homeHrefByLocale -CurrentLocale $localeCode
    architectureImageSrc = Html $architectureImageHref
    architectureImageUrl = Html $architectureImageHref
    heroEyebrow = Html $localeData.home.heroEyebrow
    heroTitle = Html $localeData.home.heroTitle
    heroTagline = Html $localeData.home.heroTagline
    heroLead = Html $localeData.home.heroLead
    heroHighlights = New-HeroHighlights -Highlights $localeData.home.heroHighlights
    runtimePanelLabel = Html $localeData.home.runtimePanelLabel
    runtimePanelKicker = Html $localeData.home.runtimePanelKicker
    runtimePanelTitle = Html $localeData.home.runtimePanelTitle
    runtimeSteps = New-RuntimeSteps -Steps $localeData.home.runtimeSteps
    runtimePanelNote = Html $localeData.home.runtimePanelNote
    actionsLabel = Html $localeData.home.actionsLabel
    heroActions = @"
            <a class="primary-action" href="$(Html $docsHref)">$(Html $localeData.home.primaryAction)</a>
            <a class="secondary-action" href="$(Html $architectureHref)">$(Html $localeData.home.secondaryAction)</a>
            <a class="secondary-action" href="#development-progress">$(Html $localeData.home.progressAction)</a>
            <a class="secondary-action" href="#feedback">$(Html $localeData.nav.feedback)</a>
"@
    progressEyebrow = Html $localeData.home.progressEyebrow
    progressTitle = Html $localeData.home.progressTitle
    progressCurrentLabel = Html $localeData.home.progressCurrentLabel
    progressCurrentKicker = Html $localeData.home.progressCurrentKicker
    progressCurrentTitle = Html $localeData.home.progressCurrentTitle
    progressCurrentBody = Html $localeData.home.progressCurrentBody
    progressTimelineLabel = Html $localeData.home.progressTimelineLabel
    progressStages = New-ProgressStages -Stages $localeData.home.progressStages
    progressNextTitle = Html $localeData.home.progressNextTitle
    progressNextItems = New-ProgressNextItems -Items $localeData.home.progressNextItems
    progressRoadmapHref = Html $progressRoadmapHref
    progressRoadmapLabel = Html $localeData.home.progressRoadmapLabel
    featuredBlogEyebrow = Html $localeData.home.featuredBlogEyebrow
    featuredBlogTitle = Html $localeData.home.featuredBlogTitle
    featuredBlogBody = Html $localeData.home.featuredBlogBody
    featuredBlogHref = Html $blogHref
    featuredBlogAction = Html $localeData.home.featuredBlogAction
    featuredBlogCards = New-FeaturedBlogCards -Manifest $manifest -Posts $localePosts -Categories $blogCategories -LocaleData $localeData -LocaleCode $localeCode -Context $context
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
  navLinks = New-NavLinks -LocaleData $defaultLocaleData -HomeHref "../" -DocsHref "./" -BlogHref "../blog/" -ArchitectureHref "en/architecture-overview.md" -FeedbackHref "../#feedback"
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

foreach ($locale in $locales) {
  $localeCode = [string]$locale.code
  $localeData = Read-JsonFile -Path (Join-Path $LocaleRoot "$localeCode.json")
  $localePosts = @($blogPosts | Where-Object { $_.locale -eq $localeCode })
  $readmePath = Get-DocumentPath -Manifest $manifest -DocumentId "readme" -LocaleCode $localeCode
  $architecturePath = Get-DocumentPath -Manifest $manifest -DocumentId "architecture-overview" -LocaleCode $localeCode

  $blogIndexPath = Get-BlogIndexSitePath -Manifest $manifest -LocaleCode $localeCode
  $blogIndexFromKind = "blog-index"
  $blogIndexTokens = @{
    lang = Html $localeCode
    dir = Html $locale.dir
    title = Html $localeData.blog.metaTitle
    description = Html $localeData.blog.metaDescription
    canonicalUrl = Html (Join-SiteUrl -BaseUrl $manifest.siteUrl -Path $blogIndexPath)
    alternateLinks = New-BlogAlternateLinks -Manifest $manifest -Locales $locales -PageKind "index" -Posts $blogPosts
    assetPrefix = Html (Get-BlogAssetPrefix -Manifest $manifest -LocaleCode $localeCode -PageKind $blogIndexFromKind)
    homeUrl = Html (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $blogIndexFromKind -TargetSitePath "/")
    brandTagline = Html $localeData.brand.tagline
    navLabel = Html $localeData.nav.label
    navLinks = New-NavLinks `
      -LocaleData $localeData `
      -HomeHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $blogIndexFromKind -TargetSitePath "/") `
      -DocsHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $blogIndexFromKind -TargetSitePath "/$readmePath") `
      -BlogHref "./" `
      -ArchitectureHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $blogIndexFromKind -TargetSitePath "/$architecturePath") `
      -FeedbackHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $blogIndexFromKind -TargetSitePath "/#feedback")
    languageSwitch = New-BlogLanguageSwitch -Manifest $manifest -Locales $locales -CurrentLocale $localeCode -FromKind $blogIndexFromKind -TargetKind "index" -Posts $blogPosts
    heroEyebrow = Html $localeData.blog.heroEyebrow
    heroTitle = Html $localeData.blog.heroTitle
    heroLead = Html $localeData.blog.heroLead
    categoriesTitle = Html $localeData.blog.categoriesTitle
    allPostsTitle = Html $localeData.blog.allPostsTitle
    categoryLinks = New-BlogCategoryLinks -Manifest $manifest -Categories $blogCategories -Posts $blogPosts -LocaleCode $localeCode -FromKind $blogIndexFromKind
    postCards = New-BlogPostCards -Manifest $manifest -Posts $localePosts -Categories $blogCategories -LocaleData $localeData -LocaleCode $localeCode -FromKind $blogIndexFromKind
    footerAuthor = Html $localeData.footer.author
    footerFeedback = Html $localeData.footer.feedback
    footerSubmit = Html $localeData.footer.submit
    feedbackUrl = Html (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $blogIndexFromKind -TargetSitePath "/#feedback")
  }

  Write-Utf8NoBom -Path (Get-BlogIndexOutputPath -Manifest $manifest -LocaleCode $localeCode) -Content (Apply-Template -Template $blogIndexTemplate -Tokens $blogIndexTokens)

  foreach ($category in $blogCategories) {
    $categoryId = [string]$category.id
    $categoryLabel = Get-CategoryLabel -Category $category -LocaleCode $localeCode
    $categoryPosts = @($localePosts | Where-Object { $_.category -eq $categoryId })
    $categorySitePath = Get-BlogCategorySitePath -Manifest $manifest -LocaleCode $localeCode -CategoryId $categoryId
    $categoryFromKind = "blog-category"
    $categoryTokens = @{
      lang = Html $localeCode
      dir = Html $locale.dir
      title = Html "$categoryLabel | $($localeData.blog.metaTitle)"
      description = Html $localeData.blog.metaDescription
      canonicalUrl = Html (Join-SiteUrl -BaseUrl $manifest.siteUrl -Path $categorySitePath)
      alternateLinks = New-BlogAlternateLinks -Manifest $manifest -Locales $locales -PageKind "category" -CategoryId $categoryId -Posts $blogPosts
      assetPrefix = Html (Get-BlogAssetPrefix -Manifest $manifest -LocaleCode $localeCode -PageKind $categoryFromKind)
      homeUrl = Html (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $categoryFromKind -TargetSitePath "/")
      brandTagline = Html $localeData.brand.tagline
      navLabel = Html $localeData.nav.label
      navLinks = New-NavLinks `
        -LocaleData $localeData `
        -HomeHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $categoryFromKind -TargetSitePath "/") `
        -DocsHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $categoryFromKind -TargetSitePath "/$readmePath") `
        -BlogHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $categoryFromKind -TargetSitePath $blogIndexPath) `
        -ArchitectureHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $categoryFromKind -TargetSitePath "/$architecturePath") `
        -FeedbackHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $categoryFromKind -TargetSitePath "/#feedback")
      languageSwitch = New-BlogLanguageSwitch -Manifest $manifest -Locales $locales -CurrentLocale $localeCode -FromKind $categoryFromKind -TargetKind "category" -CategoryId $categoryId -Posts $blogPosts
      heroEyebrow = Html $localeData.blog.heroEyebrow
      categoryTitle = Html $categoryLabel
      heroLead = Html $localeData.blog.heroLead
      allPostsTitle = Html $localeData.blog.allPostsTitle
      postCards = New-BlogPostCards -Manifest $manifest -Posts $categoryPosts -Categories $blogCategories -LocaleData $localeData -LocaleCode $localeCode -FromKind $categoryFromKind
      blogIndexHref = Html (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $categoryFromKind -TargetSitePath $blogIndexPath)
      backToBlog = Html $localeData.blog.backToBlog
      footerAuthor = Html $localeData.footer.author
      footerFeedback = Html $localeData.footer.feedback
      footerSubmit = Html $localeData.footer.submit
      feedbackUrl = Html (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $categoryFromKind -TargetSitePath "/#feedback")
    }

    Write-Utf8NoBom -Path (Get-BlogCategoryOutputPath -Manifest $manifest -LocaleCode $localeCode -CategoryId $categoryId) -Content (Apply-Template -Template $blogCategoryTemplate -Tokens $categoryTokens)
  }

  foreach ($post in $localePosts) {
    $postFromKind = "blog-post"
    $postSitePath = Get-BlogPostSitePath -Manifest $manifest -LocaleCode $localeCode -Slug ([string]$post.slug)
    $category = @($blogCategories | Where-Object { $_.id -eq $post.category }) | Select-Object -First 1
    $categoryLabel = Get-CategoryLabel -Category $category -LocaleCode $localeCode
    $categoryPath = Get-BlogCategorySitePath -Manifest $manifest -LocaleCode $localeCode -CategoryId ([string]$post.category)
    $contentPath = Join-Path $Root (([string]$post.contentPath) -replace "/", [System.IO.Path]::DirectorySeparatorChar)
    $content = Get-Content -Raw -Encoding UTF8 -LiteralPath $contentPath
    $postTokens = @{
      lang = Html $localeCode
      dir = Html $locale.dir
      title = Html "$($post.title) | $($localeData.blog.metaTitle)"
      description = Html $post.description
      canonicalUrl = Html (Join-SiteUrl -BaseUrl $manifest.siteUrl -Path $postSitePath)
      date = Html $post.date
      categoryName = Html $categoryLabel
      alternateLinks = New-BlogAlternateLinks -Manifest $manifest -Locales $locales -PageKind "post" -Post $post -Posts $blogPosts
      assetPrefix = Html (Get-BlogAssetPrefix -Manifest $manifest -LocaleCode $localeCode -PageKind $postFromKind)
      homeUrl = Html (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $postFromKind -TargetSitePath "/")
      brandTagline = Html $localeData.brand.tagline
      navLabel = Html $localeData.nav.label
      navLinks = New-NavLinks `
        -LocaleData $localeData `
        -HomeHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $postFromKind -TargetSitePath "/") `
        -DocsHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $postFromKind -TargetSitePath "/$readmePath") `
        -BlogHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $postFromKind -TargetSitePath $blogIndexPath) `
        -ArchitectureHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $postFromKind -TargetSitePath "/$architecturePath") `
        -FeedbackHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $postFromKind -TargetSitePath "/#feedback")
      languageSwitch = New-BlogLanguageSwitch -Manifest $manifest -Locales $locales -CurrentLocale $localeCode -FromKind $postFromKind -TargetKind "post" -Post $post -Posts $blogPosts
      heroEyebrow = Html $localeData.blog.heroEyebrow
      postTitle = Html $post.title
      postDescription = Html $post.description
      dateLabel = Html $localeData.blog.dateLabel
      categoryLabel = Html $localeData.blog.categoryLabel
      categoryHref = Html (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $postFromKind -TargetSitePath $categoryPath)
      content = $content
      blogIndexHref = Html (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $postFromKind -TargetSitePath $blogIndexPath)
      backToBlog = Html $localeData.blog.backToBlog
      footerAuthor = Html $localeData.footer.author
      footerFeedback = Html $localeData.footer.feedback
      footerSubmit = Html $localeData.footer.submit
      feedbackUrl = Html (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $postFromKind -TargetSitePath "/#feedback")
    }

    Write-Utf8NoBom -Path (Get-BlogPostOutputPath -Manifest $manifest -LocaleCode $localeCode -Slug ([string]$post.slug)) -Content (Apply-Template -Template $blogPostTemplate -Tokens $postTokens)
  }
}

Write-Host "Generated localized website and blog pages for $($locales.Count) locale(s)."
