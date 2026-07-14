#!/usr/bin/env pwsh
# 校验 i18n manifest、locale JSON、文档/资产映射以及已生成首页、docs 与博客页面的一致性。
# 脚本只读取仓库内容；任一结构、路径、canonical/hreflang、模板或讨论嵌入契约不满足时，
# 通过 Fail 抛出异常并以非零状态退出，使 CI 不会发布语言入口不完整或相互矛盾的站点。
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# 所有相对路径检查锚定仓库根，验证结果不受调用者当前目录影响。
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$ManifestPath = Join-Path $Root "docs/_i18n/manifest.json"
$LocaleRoot = Join-Path $Root "website/_i18n"
$WebsiteRoot = Join-Path $Root "website"
$DocsRoot = Join-Path $Root "docs"
$BlogDataPath = Join-Path $Root "website/_blog/posts.json"

# 以 UTF-8 读取并解析 JSON；文件或语法错误由 Stop 策略直接终止验证。
function Read-JsonFile {
  param([Parameter(Mandatory = $true)][string]$Path)
  return Get-Content -Raw -Encoding UTF8 -LiteralPath $Path | ConvertFrom-Json
}

# 统一生成带 i18n 前缀的终止错误，保留具体 locale/文档/页面上下文。
function Fail {
  param([Parameter(Mandatory = $true)][string]$Message)
  throw "i18n validation failed: $Message"
}

# 将 manifest 使用的 `/` 仓库相对路径转换为当前平台路径并检查存在性。
# RelativePath 必须来自受信 manifest/博客数据；函数不接受站点 URL。
function Test-RepoPath {
  param([Parameter(Mandatory = $true)][string]$RelativePath)
  $path = Join-Path $Root ($RelativePath -replace "/", [System.IO.Path]::DirectorySeparatorChar)
  return Test-Path -LiteralPath $path
}

# 安全读取 PSCustomObject 动态属性；缺失返回 null，供翻译状态和可选映射做显式判断。
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

# 返回 locale 感知的博客索引 URL 路径；默认语言不带 locale 前缀。
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

# 根据已校验 CategoryId 构造语言分类页 URL 路径。
function Get-BlogCategorySitePath {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$LocaleCode,
    [Parameter(Mandatory = $true)][string]$CategoryId
  )

  return "$(Get-BlogIndexSitePath -Manifest $Manifest -LocaleCode $LocaleCode)category/$CategoryId/"
}

# 根据 locale 与已校验 slug 构造文章 URL 路径。
function Get-BlogPostSitePath {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$LocaleCode,
    [Parameter(Mandatory = $true)][string]$Slug
  )

  return "$(Get-BlogIndexSitePath -Manifest $Manifest -LocaleCode $LocaleCode)$Slug/"
}

# 将博客索引 URL 语义映射到实际静态 index.html 路径。
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

# 将 locale/category 映射到实际分类页输出路径。
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

# 将 locale/slug 映射到实际文章页输出路径。
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

# 合并 canonical 基址与站点路径，并把边界规范为恰好一个斜线。
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

# 确保生成 HTML 不含未解析的 {{token}}；发现首个占位符即报告页面路径并终止。
function Assert-NoTemplateToken {
  param(
    [Parameter(Mandatory = $true)][string]$Html,
    [Parameter(Mandatory = $true)][string]$Path
  )

  if ($Html -match "{{[^}]+}}") {
    Fail "Generated HTML contains unresolved token '$($Matches[0])': $Path"
  }
}

# 第一阶段验证 manifest 根字段和启用 locale；后续所有映射都依赖这组权威数据。
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

# 对每个启用 locale 同时校验语言元数据、locale JSON 必填成员和已生成首页契约。
foreach ($locale in $locales) {
  # 这些成员决定目录、语言切换可见文本、排版方向和翻译层级，均不可回退为空值。
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
  # 顶层 locale 数据是模板 token 分区；缺任一分区都会造成页面部分未本地化。
  foreach ($path in @("meta", "brand", "nav", "home", "discussion", "docsIndex", "blog", "feedback", "footer")) {
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
  if ($homeHtml -notmatch 'id="discussion"') {
    Fail "Generated home page for '$($locale.code)' is missing the discussion section."
  }
  if ($homeHtml -notmatch 'href="#discussion"') {
    Fail "Generated home page for '$($locale.code)' is missing the discussion navigation link."
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
  if ($homeHtml -notmatch 'https://giscus\.app/client\.js') {
    Fail "Generated home page for '$($locale.code)' is missing the giscus embed script."
  }
  if ($homeHtml -match 'REPLACE_WITH_') {
    Fail "Generated home page for '$($locale.code)' still contains a giscus placeholder."
  }
  if ($homeHtml -notmatch 'data-category-id="DIC_kwDOS4ZJEM4C_Tf8"') {
    Fail "Generated home page for '$($locale.code)' has incorrect giscus category id."
  }
  if ($homeHtml -notmatch 'data-mapping="specific"') {
    Fail "Generated home page for '$($locale.code)' must use a fixed giscus mapping."
  }
  if ($homeHtml -notmatch 'data-term="Eva-CLI site discussion"') {
    Fail "Generated home page for '$($locale.code)' must use the shared site discussion term."
  }
  if ($homeHtml -match 'data-eva-chat|chat/chat-app\.js|data-chat-action=|firebase-messaging-sw\.js') {
    Fail "Generated home page for '$($locale.code)' still contains Firebase chat markup."
  }

  # Giscus 当前只配置中文或英文 UI；其他 locale 显式回退英文，讨论线程身份仍保持共享。
  $expectedGiscusLang = if ($locale.code -eq "zh-CN") { "zh-CN" } else { "en" }
  if ($homeHtml -notmatch "data-lang=`"$([regex]::Escape($expectedGiscusLang))`"") {
    Fail "Generated home page for '$($locale.code)' has incorrect giscus language."
  }
}

# 第二阶段校验每个文档的 source、各非默认语言状态和 translation 路径一致性。
# status=missing 允许无路径；其他状态必须有真实译文文件，防止状态与文件事实背离。
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

# 资产采用与文档相同的状态/路径规则，确保生成器的 source 回退有可用文件。
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

# Architecture 图是首页关键本地化资产，除 manifest 映射有效外还要验证生成 HTML 实际引用。
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

# 第三阶段验证 /docs/ 只有英文 canonical index，避免部署时覆盖真实 docs 内容树。
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

# 第四阶段先验证博客元数据全局约束，再逐 locale 验证生成页和交叉语言链接。
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
# HashSet 用于把分类 ID 全局唯一性与 slug 的 locale 内唯一性转成确定失败。
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

# 所有文章必须引用启用 locale、已知分类、存在的内容文件和可解析日期。
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

# 验证博客索引、每个分类和每篇文章的输出存在、token 已解析、canonical 正确；
# 文章还必须为所有真实存在的同 ID 语言 sibling 提供 hreflang。
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
