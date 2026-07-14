#!/usr/bin/env pwsh
# 根据 i18n manifest、语言数据、博客元数据和 HTML 模板生成本地化静态站点。
# 脚本会写入 website 下的生成页面；任一输入缺失、模板 token 未解析或内容契约无效时立即抛错，
# 从而避免发布半生成站点。所有输出统一为无 BOM UTF-8，保证跨平台部署结果稳定。
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# 所有输入和输出都锚定仓库根，禁止依赖调用者当前工作目录。
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$ManifestPath = Join-Path $Root "docs/_i18n/manifest.json"
$TemplateRoot = Join-Path $Root "website/_templates"
$LocaleRoot = Join-Path $Root "website/_i18n"
$WebsiteRoot = Join-Path $Root "website"
$BlogDataPath = Join-Path $Root "website/_blog/posts.json"
# Giscus 参数是所有语言页面共享的固定讨论线程身份；不能随 locale 改变，否则评论会分裂。
$GiscusRepo = "Yetmos/Eva-CLI"
$GiscusRepoId = "R_kgDOS4ZJEA"
$GiscusCategory = "General"
$GiscusCategoryId = "DIC_kwDOS4ZJEM4C_Tf8"
$GiscusTerm = "Eva-CLI site discussion"

# 以 UTF-8 一次性读取并解析 JSON；解析失败直接终止生成，调用方不会收到部分对象。
function Read-JsonFile {
  param([Parameter(Mandatory = $true)][string]$Path)
  return Get-Content -Raw -Encoding UTF8 -LiteralPath $Path | ConvertFrom-Json
}

# 将完整页面原子式写入指定路径使用的基础出口，并按需创建父目录。
# Content 必须已完成模板替换；无 BOM 编码避免静态托管和前端工具链出现首字符差异。
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

# 对进入 HTML 文本或属性的动态值做实体编码；null 规范为空串，便于可选 token 安全渲染。
function Html {
  param([AllowNull()][string]$Value)
  if ($null -eq $Value) {
    return ""
  }

  return [System.Net.WebUtility]::HtmlEncode($Value)
}

# 使用显式 token 映射渲染模板。
# 输入 Tokens 的值必须已按使用场景完成 HTML 编码；替换后若仍有 {{...}}，立即失败，
# 防止遗漏字段以占位符形式发布到生产站点。
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

# 按名称读取 PSCustomObject 的动态属性；属性不存在返回 null，而不是触发 StrictMode 异常。
# 该语义用于 locale/translation 可选键的显式回退判断。
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

# 按稳定 ID 从 manifest 查找文档；未注册属于生成配置错误，不能静默拼接路径。
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

# 按稳定 ID 从 manifest 查找本地化资产；缺失映射时立即终止生成。
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

# 解析某语言的文档仓库相对路径。
# 默认语言始终使用 source；非默认语言仅在 translation 非空时使用译文，否则回退 source。
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

# 解析某语言的资产仓库相对路径，回退规则与文档一致，保证缺译资产仍可访问。
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

# 按页面所在深度把仓库文档路径转换为相对 href。
# Context 被限制为三种已知目录布局，避免任意深度计算导致链接越级或发布后失效。
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

# 按根首页、语言首页或 docs 索引上下文转换站内资源路径；Path 不应是外部 URL。
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

# 规范化站点基址与路径之间的单个斜线，生成 canonical/hreflang 使用的绝对 URL。
# BaseUrl 的尾斜线和 Path 的首斜线都可有可无，但不会修改 Path 内部结构。
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

# 返回某语言首页的站点绝对路径；默认语言占据根路径，其他语言使用 /<locale>/。
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

# 为首页或 docs 索引生成所有启用语言的 hreflang，并追加固定 x-default。
# docs 非默认语言链接依据 manifest 文档映射；缺译时沿用 Get-DocumentPath 的 source 回退。
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

# 返回 locale 感知的博客索引站点路径，保持默认语言不带 locale 前缀。
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

# 在对应语言博客根下构造分类站点路径；CategoryId 必须已由博客数据校验通过。
function Get-BlogCategorySitePath {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$LocaleCode,
    [Parameter(Mandatory = $true)][string]$CategoryId
  )

  return "$(Get-BlogIndexSitePath -Manifest $Manifest -LocaleCode $LocaleCode)category/$CategoryId/"
}

# 在对应语言博客根下构造文章站点路径；Slug 的唯一性由 Assert-BlogData 保证。
function Get-BlogPostSitePath {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$LocaleCode,
    [Parameter(Mandatory = $true)][string]$Slug
  )

  return "$(Get-BlogIndexSitePath -Manifest $Manifest -LocaleCode $LocaleCode)$Slug/"
}

# 将语言博客索引映射到实际 index.html 输出位置。
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

# 将语言/分类组合映射到静态分类页输出位置。
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

# 将语言/slug 组合映射到静态文章页输出位置。
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

# 从博客索引、分类或文章页计算到任意站内绝对路径的相对 href。
# FromKind 与默认/非默认 locale 共同决定目录深度；根路径单独处理以保留正确尾斜线。
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

# 复用博客相对链接计算得到回到站点根的资源前缀，供 CSS/JS/图片 token 使用。
function Get-BlogAssetPrefix {
  param(
    [Parameter(Mandatory = $true)]$Manifest,
    [Parameter(Mandatory = $true)][string]$LocaleCode,
    [Parameter(Mandatory = $true)][ValidateSet("blog-index", "blog-category", "blog-post")][string]$PageKind
  )

  $rootPath = Get-BlogRelativeHref -Manifest $Manifest -FromLocale $LocaleCode -FromKind $PageKind -TargetSitePath "/"
  return $rootPath
}

# 读取分类的 locale 标签；缺少译名时回退稳定 category ID，而不生成空导航文本。
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

# 为博客索引、分类或文章生成 canonical sibling hreflang 集合。
# 文章只为真实存在的同 ID 译文输出链接；x-default 优先指向默认语言，否则回退当前文章。
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

# 构造博客语言切换映射；目标文章缺少译文时回退该语言博客索引，避免死链。
# FromKind 描述当前页面深度，TargetKind 描述切换后的页面语义，两者不能混用。
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

  # 先建立完整 locale -> 相对 href 映射，再交给通用语言切换器统一渲染 aria/hreflang。
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

# 生成博客分类导航，并按当前语言统计文章数；链接相对深度由 FromKind 统一计算。
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

# 按日期降序、标题升序生成文章卡片，保证同日期内容构建结果确定。
# 空集合输出本地化 empty state，而不是返回空容器。
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

# 生成首页有限数量的最新文章卡片。
# Context 只允许根首页或语言首页，因为两者的 locale 前缀剥离和资源相对深度不同。
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

# 在任何页面写入前验证博客元数据的全局一致性。
# 要求至少一个分类、分类 ID 全局唯一、slug 在各 locale 内唯一、文章语言已启用、分类存在、
# 内容文件可读且日期可解析；失败通过 throw 终止整个生成，避免产生部分博客树。
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

# 从完整 locale -> href 映射渲染语言切换器，并为当前语言添加 aria-current。
# HrefByLocale 必须覆盖 Locales 中每个 code，调用方负责先处理缺译回退。
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

# 使用本地化标签和调用方已归一化的 href 生成全站主导航。
function New-NavLinks {
  param(
    [Parameter(Mandatory = $true)]$LocaleData,
    [Parameter(Mandatory = $true)][string]$HomeHref,
    [Parameter(Mandatory = $true)][string]$DocsHref,
    [Parameter(Mandatory = $true)][string]$BlogHref,
    [Parameter(Mandatory = $true)][string]$ArchitectureHref,
    [Parameter(Mandatory = $true)][string]$DiscussionHref,
    [Parameter(Mandatory = $true)][string]$FeedbackHref
  )

  return @"
        <a href="$(Html $HomeHref)">$(Html $LocaleData.nav.home)</a>
        <a href="$(Html $DocsHref)">$(Html $LocaleData.nav.docs)</a>
        <a href="$(Html $BlogHref)">$(Html $LocaleData.nav.blog)</a>
        <a href="$(Html $ArchitectureHref)">$(Html $LocaleData.nav.architecture)</a>
        <a href="$(Html $DiscussionHref)">$(Html $LocaleData.nav.discussion)</a>
        <a href="$(Html $FeedbackHref)">$(Html $LocaleData.nav.feedback)</a>
        <a href="https://github.com/Yetmos/Eva-CLI">GitHub</a>
"@
}

# 生成共享 Giscus 讨论线程嵌入代码；仅 UI 语言随 locale 变化，repo/category/term 固定。
function New-DiscussionEmbed {
  param(
    [Parameter(Mandatory = $true)][string]$LocaleCode
  )

  $giscusLang = if ($LocaleCode -eq "zh-CN") { "zh-CN" } else { "en" }

  return @"
          <script src="https://giscus.app/client.js"
            data-repo="$(Html $GiscusRepo)"
            data-repo-id="$(Html $GiscusRepoId)"
            data-category="$(Html $GiscusCategory)"
            data-category-id="$(Html $GiscusCategoryId)"
            data-mapping="specific"
            data-term="$(Html $GiscusTerm)"
            data-strict="0"
            data-reactions-enabled="1"
            data-emit-metadata="0"
            data-input-position="top"
            data-theme="preferred_color_scheme"
            data-lang="$(Html $giscusLang)"
            crossorigin="anonymous"
            async>
          </script>
"@
}

# 将 locale 数据中的文档卡片 ID 解析为 manifest 路径并生成首页链接。
# Context 限制 href 深度，文档缺译时沿用 manifest source 回退。
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

# 对每个本地化反馈分类做 HTML 编码并生成 select option。
function New-FeedbackOptions {
  param([Parameter(Mandatory = $true)]$Options)

  $items = New-Object System.Collections.Generic.List[string]
  foreach ($option in $Options) {
    $items.Add("                <option value=`"$(Html $option)`">$(Html $option)</option>")
  }

  return ($items -join "`n")
}

# 将首页亮点文本渲染为安全的独立 span 列表。
function New-HeroHighlights {
  param([Parameter(Mandatory = $true)]$Highlights)

  $items = New-Object System.Collections.Generic.List[string]
  foreach ($highlight in $Highlights) {
    $items.Add("            <span>$(Html $highlight)</span>")
  }

  return ($items -join "`n")
}

# 将本地化 runtime 步骤的 label/description 映射为有序展示项。
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

# 生成开发进度时间线，并只接受 CSS/语义已定义的四种状态。
# 未知状态立即抛错，避免生成缺样式且含义不明的进度节点。
function New-ProgressStages {
  param([Parameter(Mandatory = $true)]$Stages)

  # 此集合同时是内容 schema 与 CSS class 白名单，新增状态必须同步模板样式和验证器。
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

# 将后续工作文本编码为列表项，保持 locale 文件中的顺序。
function New-ProgressNextItems {
  param([Parameter(Mandatory = $true)]$Items)

  $output = New-Object System.Collections.Generic.List[string]
  foreach ($item in $Items) {
    $output.Add("            <li>$(Html $item)</li>")
  }

  return ($output -join "`n")
}

# 生成 docs 索引的语言入口，并根据 default/detailed-source/translation 标注内容权威层级。
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

# 为 docs 索引的每个文档生成所有启用语言链接；路径解析统一遵守 manifest 回退规则。
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

# 第一阶段只读取并验证全部共享输入；Assert-BlogData 通过前不写任何页面。
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

# 第二阶段按 locale 生成首页。默认语言写根 index，其他语言写 <locale>/index。
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

  # 首页语言切换使用站点绝对路径；浏览器会从当前页面正确解析根/locale 入口。
  $homeHrefByLocale = @{}
  foreach ($targetLocale in $locales) {
    $homeHrefByLocale[[string]$targetLocale.code] = Get-LocaleHomePath -Manifest $manifest -LocaleCode ([string]$targetLocale.code)
  }

  $readmePath = Get-DocumentPath -Manifest $manifest -DocumentId "readme" -LocaleCode $localeCode
  $architecturePath = Get-DocumentPath -Manifest $manifest -DocumentId "architecture-overview" -LocaleCode $localeCode
  $progressRoadmapPath = Get-DocumentPath -Manifest $manifest -DocumentId "v1.x-incomplete-feature-inventory" -LocaleCode $localeCode
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

  # 模板 token 是生成首页的完整契约：动态文本先 Html 编码，结构化 HTML 由 New-* 函数生成。
  # Apply-Template 会拒绝任何遗漏 token，因此新增模板字段必须在这里显式接线。
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
    navLinks = New-NavLinks -LocaleData $localeData -HomeHref "./" -DocsHref $docsHref -BlogHref $blogHref -ArchitectureHref $architectureHref -DiscussionHref "#discussion" -FeedbackHref "#feedback"
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
            <a class="secondary-action" href="#discussion">$(Html $localeData.nav.discussion)</a>
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
    discussionEyebrow = Html $localeData.discussion.eyebrow
    discussionTitle = Html $localeData.discussion.title
    discussionBody = Html $localeData.discussion.body
    discussionEmbed = New-DiscussionEmbed -LocaleCode $localeCode
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

# Docs 索引固定使用默认语言元数据，作为 /docs/ 的 canonical 入口。
$defaultLocaleCode = [string]$manifest.defaultLocale
$defaultLocale = @($locales | Where-Object { $_.code -eq $defaultLocaleCode }) | Select-Object -First 1
$defaultLocaleData = Read-JsonFile -Path (Join-Path $LocaleRoot "$defaultLocaleCode.json")
$docsHrefByLocale = @{}
# Docs 索引语言切换映射到各语言 README；默认语言仍使用 /docs/ 当前页。
foreach ($targetLocale in $locales) {
  $targetCode = [string]$targetLocale.code
  $docsHrefByLocale[$targetCode] = if ($targetCode -eq $manifest.defaultLocale) {
    "/docs/"
  } else {
    "/$(Get-DocumentPath -Manifest $manifest -DocumentId "readme" -LocaleCode $targetCode)"
  }
}

# Docs 索引 token 映射只包含已编码文本或生成片段，并由未解析 token 检查兜底。
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
  navLinks = New-NavLinks -LocaleData $defaultLocaleData -HomeHref "../" -DocsHref "./" -BlogHref "../blog/" -ArchitectureHref "en/architecture/architecture-overview.md" -DiscussionHref "../#discussion" -FeedbackHref "../#feedback"
  languageSwitch = New-LanguageSwitch -Locales $locales -HrefByLocale $docsHrefByLocale -CurrentLocale $defaultLocaleCode
  heroEyebrow = Html $defaultLocaleData.docsIndex.heroEyebrow
  heroTitle = Html $defaultLocaleData.docsIndex.heroTitle
  heroLead = Html $defaultLocaleData.docsIndex.heroLead
  actionsLabel = Html $defaultLocaleData.docsIndex.actionsLabel
  heroActions = @"
            <a class="primary-action" href="en/architecture/architecture-overview.md">$(Html $defaultLocaleData.docsIndex.primaryAction)</a>
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

# 第三阶段为每个 locale 生成博客索引、全部分类页和该语言文章页。
foreach ($locale in $locales) {
  $localeCode = [string]$locale.code
  $localeData = Read-JsonFile -Path (Join-Path $LocaleRoot "$localeCode.json")
  $localePosts = @($blogPosts | Where-Object { $_.locale -eq $localeCode })
  $readmePath = Get-DocumentPath -Manifest $manifest -DocumentId "readme" -LocaleCode $localeCode
  $architecturePath = Get-DocumentPath -Manifest $manifest -DocumentId "architecture-overview" -LocaleCode $localeCode

  $blogIndexPath = Get-BlogIndexSitePath -Manifest $manifest -LocaleCode $localeCode
  $blogIndexFromKind = "blog-index"
  # 博客索引 token 统一使用 locale 感知 canonical、相对导航和语言切换映射。
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
      -DiscussionHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $blogIndexFromKind -TargetSitePath "/#discussion") `
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
    # 分类页 token 的相对链接深度不同于索引页，必须全部基于 categoryFromKind 计算。
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
        -DiscussionHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $categoryFromKind -TargetSitePath "/#discussion") `
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
    # 文章正文来自受 Assert-BlogData 校验的内容文件；content 保留为可信 HTML 片段，
    # 其余动态 metadata 全部进行 HTML 编码并生成 canonical/hreflang。
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
        -DiscussionHref (Get-BlogRelativeHref -Manifest $manifest -FromLocale $localeCode -FromKind $postFromKind -TargetSitePath "/#discussion") `
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
