# 静态网站博客与分类功能方案

## 背景

当前 Eva-CLI 官网是一个低依赖静态站点：

- `website/_templates/home.html` 和 `website/_templates/docs-index.html` 维护页面模板。
- `website/_i18n/en.json` 和 `website/_i18n/zh-CN.json` 维护多语言文案。
- `scripts/build-site-i18n.ps1` 从模板和 locale JSON 生成静态 HTML。
- `.github/workflows/pages.yml` 将 `website/`、`docs/` 和 `assets/` 组装为 GitHub Pages 发布产物。

这套结构适合继续扩展为“静态博客”，不需要引入数据库、CMS、Node 构建链或后端服务。

## 结论

推荐在现有 PowerShell 静态生成流程上扩展博客功能：

> 使用结构化 JSON 维护博客元数据，使用 HTML 内容片段维护文章正文，由 `build-site-i18n.ps1` 生成博客首页、分类页和文章详情页。

第一版不建议引入 Markdown 解析器或独立静态站点框架。当前仓库没有 npm/package 配置，继续保持零运行时依赖更符合项目现状。

## 目标效果

生成后的站点支持：

- 博客首页：`/blog/`
- 博客分类页：`/blog/category/<category>/`
- 博客文章页：`/blog/<slug>/`
- 中文博客首页：`/zh-CN/blog/`
- 中文分类页：`/zh-CN/blog/category/<category>/`
- 中文文章页：`/zh-CN/blog/<slug>/`

导航增加 `Blog` / `博客` 入口。博客列表支持按分类浏览，文章详情页带 canonical、Open Graph、Twitter card 和多语言 `hreflang`。

## 推荐内容结构

新增博客元数据文件：

```text
website/_blog/posts.json
```

建议结构：

```json
{
  "categories": [
    {
      "id": "runtime",
      "labels": {
        "en": "Runtime",
        "zh-CN": "运行时"
      }
    }
  ],
  "posts": [
    {
      "id": "eva-runtime-notes",
      "locale": "en",
      "slug": "eva-runtime-notes",
      "title": "Eva Runtime Notes",
      "description": "Design notes about Eva-CLI runtime boundaries.",
      "date": "2026-06-17",
      "category": "runtime",
      "contentPath": "website/_blog/content/en/eva-runtime-notes.html"
    },
    {
      "id": "eva-runtime-notes",
      "locale": "zh-CN",
      "slug": "eva-runtime-notes",
      "title": "Eva 运行时笔记",
      "description": "关于 Eva-CLI 运行时边界的设计笔记。",
      "date": "2026-06-17",
      "category": "runtime",
      "contentPath": "website/_blog/content/zh-CN/eva-runtime-notes.html"
    }
  ]
}
```

文章正文使用 HTML 片段：

```text
website/_blog/content/en/eva-runtime-notes.html
website/_blog/content/zh-CN/eva-runtime-notes.html
```

示例正文：

```html
<p>Eva-CLI keeps runtime ownership in Rust and puts hot-reloadable behavior in Lua.</p>

<h2>Runtime boundary</h2>
<p>The key design point is keeping policy and recovery outside hot-reloaded scripts.</p>
```

## 为什么先用 HTML 片段

当前项目没有 Markdown 到 HTML 的统一渲染链路。文档目录里的 Markdown 是直接复制到 Pages 产物中，不是经过模板渲染后发布。

如果第一版博客直接支持 Markdown 渲染，需要额外决定：

- 使用哪一个 Markdown 解析器。
- 如何接入 PowerShell 构建。
- 是否新增 Node、Python、Rust 或其他依赖。
- 代码高亮、目录、链接重写、HTML 安全策略如何处理。

这些都会扩大改造范围。HTML 片段虽然不如 Markdown 舒适，但能用最小成本落地博客功能，并保持现有发布链路稳定。

## 需要新增的模板

建议新增三个模板：

```text
website/_templates/blog-index.html
website/_templates/blog-category.html
website/_templates/blog-post.html
```

### blog-index.html

用于博客首页，展示：

- 页面标题与说明。
- 分类入口。
- 当前语言下全部文章列表。
- 每篇文章的标题、摘要、日期、分类和详情链接。

### blog-category.html

用于分类列表页，展示：

- 当前分类名称。
- 当前语言下属于该分类的文章列表。
- 返回全部博客入口。

### blog-post.html

用于文章详情页，展示：

- 标题。
- 摘要。
- 日期。
- 分类。
- 正文 HTML。
- 返回博客首页或分类页入口。

文章页不需要接入评论功能，除非后续决定每篇文章都使用 giscus 独立讨论串。

## 构建脚本改造

扩展 `scripts/build-site-i18n.ps1`：

1. 读取 `website/_blog/posts.json`。
2. 校验分类：
   - `categories[].id` 必须存在。
   - 每个分类必须提供当前启用语言的显示名称，缺失时可回退到分类 id。
3. 校验文章：
   - `id`、`locale`、`slug`、`title`、`description`、`date`、`category`、`contentPath` 必填。
   - `locale` 必须是启用语言。
   - `category` 必须存在于分类列表。
   - `contentPath` 指向的正文文件必须存在。
   - 同一语言下 slug 不能重复。
4. 按语言和日期倒序生成博客首页。
5. 按语言和分类生成分类页。
6. 为每篇文章生成详情页。
7. 同一 `id` 的不同语言文章生成 `hreflang` alternate。

路径规则建议固定为：

```text
en:    website/blog/index.html
en:    website/blog/category/<category>/index.html
en:    website/blog/<slug>/index.html
zh-CN: website/zh-CN/blog/index.html
zh-CN: website/zh-CN/blog/category/<category>/index.html
zh-CN: website/zh-CN/blog/<slug>/index.html
```

默认语言 `en` 不放在 `/en/` 前缀下，保持当前首页和文档入口规则一致。

## i18n 文案改造

在 `website/_i18n/en.json` 和 `website/_i18n/zh-CN.json` 中扩展导航：

```json
"nav": {
  "blog": "Blog"
}
```

中文：

```json
"nav": {
  "blog": "博客"
}
```

新增博客页面文案区，建议为：

```json
"blog": {
  "metaTitle": "Eva-CLI Blog",
  "metaDescription": "Updates, design notes, and implementation articles from Eva-CLI.",
  "heroEyebrow": "Blog",
  "heroTitle": "Eva-CLI Blog",
  "heroLead": "Updates, design notes, and implementation articles.",
  "categoriesTitle": "Categories",
  "allPostsTitle": "All posts",
  "categoryLabel": "Category",
  "dateLabel": "Published",
  "readMore": "Read more",
  "backToBlog": "Back to blog",
  "emptyState": "No posts yet."
}
```

中文按相同字段补齐。

`New-NavLinks` 需要增加 `BlogHref` 参数，并在所有页面导航中输出 Blog 链接。

## 样式改造

在 `website/styles.css` 中新增博客样式即可，不需要重写现有视觉体系。

建议新增样式覆盖：

- `.blog-page`
- `.blog-hero`
- `.blog-layout`
- `.blog-category-list`
- `.blog-post-list`
- `.blog-card`
- `.blog-meta`
- `.blog-article`
- `.blog-content`

设计原则：

- 复用现有 `--surface`、`--line`、`--accent`、`--muted` 等 CSS 变量。
- 卡片保持 8px 圆角，和现有站点一致。
- 移动端下分类和文章列表改为单列。
- 正文控制最大宽度，提升阅读体验。

## 校验脚本改造

扩展 `scripts/validate-i18n.ps1`：

- 校验 `website/_blog/posts.json` 存在。
- 校验所有文章正文文件存在。
- 校验文章 locale 属于启用语言。
- 校验分类 id 有效。
- 校验同一 locale 下 slug 不重复。
- 校验构建后博客首页存在。
- 校验构建后分类页存在。
- 校验构建后文章页存在。
- 校验文章页包含 canonical。
- 多语言文章存在时，校验文章页包含对应 `hreflang`。
- 校验生成 HTML 中没有残留 `{{token}}`。

继续保留现有规则：

- `website/docs` 只能包含 `index.html`，避免和 `docs/` 发布复制互相覆盖。

博客输出到 `website/blog` 和 `website/<locale>/blog`，不会影响现有 docs 发布规则。

## GitHub Pages 发布影响

当前 `.github/workflows/pages.yml` 已经执行：

```bash
cp -R website/. public/
```

因此博客页面只要生成在 `website/` 下，就会自动发布。第一版不需要修改 GitHub Actions 工作流。

需要注意：发布产物会删除：

```bash
rm -rf public/_templates public/_i18n
```

建议同步删除：

```bash
rm -rf public/_blog
```

这样正文源片段和博客元数据不会作为公开源文件额外暴露。生成后的静态 HTML 仍正常发布。

## 实施步骤

### 1. 增加博客数据与示例文章

新增：

```text
website/_blog/posts.json
website/_blog/content/en/eva-runtime-notes.html
website/_blog/content/zh-CN/eva-runtime-notes.html
```

先放 1 篇英文文章和 1 篇中文对应文章，验证完整生成链路。

### 2. 增加博客模板

新增：

```text
website/_templates/blog-index.html
website/_templates/blog-category.html
website/_templates/blog-post.html
```

模板复用现有 header、footer、language switch、nav token 结构。

### 3. 扩展构建脚本

在 `scripts/build-site-i18n.ps1` 中增加博客生成函数：

- `New-BlogIndexCards`
- `New-BlogCategoryLinks`
- `New-BlogPostCards`
- `New-BlogAlternateLinks`
- `Get-BlogHref`
- `Get-BlogIndexPath`
- `Get-BlogPostOutputPath`

也可以用更少函数实现，但要保持路径规则集中，避免模板里硬编码复杂路径。

### 4. 扩展 locale JSON

补齐：

- `nav.blog`
- `blog.*`

英文和中文字段必须一致，避免模板 token 缺失。

### 5. 扩展 CSS

增加博客页面、列表、分类和正文样式。

不要改动首页主视觉，避免把博客功能扩展变成整站重设计。

### 6. 扩展校验脚本

在 `scripts/validate-i18n.ps1` 中增加博客结构校验，并将其纳入现有验证流程。

### 7. 更新发布清理

在 `.github/workflows/pages.yml` 的清理步骤中加入：

```bash
rm -rf public/_blog
```

## 本地验证

执行：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-site-i18n.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\validate-i18n.ps1
```

检查生成文件：

```text
website/blog/index.html
website/blog/category/runtime/index.html
website/blog/eva-runtime-notes/index.html
website/zh-CN/blog/index.html
website/zh-CN/blog/category/runtime/index.html
website/zh-CN/blog/eva-runtime-notes/index.html
```

浏览器手动检查：

- 首页导航能进入博客。
- 博客首页能进入分类页。
- 博客首页和分类页能进入文章页。
- 文章页返回博客链接正常。
- 英文和中文文章页语言切换正常。
- 移动端没有文本溢出或布局重叠。

## 后续升级路径

第一版稳定后，可以再考虑：

- 支持 Markdown 正文，并引入明确的 Markdown 渲染器。
- 为文章生成 RSS/Atom。
- 为每篇文章接入独立 giscus 评论串。
- 增加文章标签 tags。
- 增加上一篇/下一篇导航。
- 增加文章目录。
- 增加草稿状态 `draft`，构建时默认跳过。

## 当前建议

先实现 JSON 元数据 + HTML 正文片段 + 静态生成的博客系统。

这是当前项目状态下成本最低、发布链路最稳定、也最容易继续演进的方案。
