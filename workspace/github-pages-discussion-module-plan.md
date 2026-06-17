# GitHub Pages 用户讨论模块低成本实现方案

## 背景

当前 Eva-CLI 官网是静态站点：

- `website/_templates/home.html` 和 `website/_templates/docs-index.html` 维护页面模板。
- `website/_i18n/en.json` 和 `website/_i18n/zh-CN.json` 维护多语言文案。
- `scripts/build-site-i18n.ps1` 从模板和 locale JSON 生成静态 HTML。
- `.github/workflows/pages.yml` 将 `website/`、`docs/` 和 `assets/` 组装为 GitHub Pages 发布产物。

这套结构适合低维护的静态发布，不适合在仓库内自建登录、数据库、评论 API 或论坛后端。

## 结论

最低成本、维护最简单的方案是：

> 使用 giscus 接入 GitHub Discussions，把网站讨论数据托管到 GitHub 仓库的 Discussions 中。

GitHub Pages 继续只负责静态页面托管。用户登录、评论存储、通知、权限和管理都交给 GitHub。

## 不推荐自建后端

不建议为了讨论模块新增 Node、PHP、数据库、Supabase、Firebase 或独立论坛服务，除非后续已经明确需要完整社区功能。

原因：

- 当前部署链路是纯静态 Pages，引入后端会增加部署面和运维成本。
- 用户身份体系需要额外设计和维护。
- 评论数据备份、反垃圾、权限管理、通知都需要额外处理。
- 对当前官网阶段而言，复杂度明显超过收益。

## 推荐方案：giscus + GitHub Discussions

giscus 是基于 GitHub Discussions 的评论/讨论组件，适合 GitHub Pages、文档站和项目主页。

官网：

<https://giscus.app/>

### 优点

- 不需要自建服务器。
- 不需要数据库。
- 用户用 GitHub 账号登录。
- 讨论内容沉淀在仓库 Discussions 中。
- GitHub 原生支持通知、表情反应、管理、锁定、删除等能力。
- 对当前静态站只需要增加 HTML script、少量 CSS 和 i18n 文案。

### 限制

- 用户必须有 GitHub 账号。
- 讨论数据绑定 GitHub 仓库。
- UI 可定制能力有限。
- 不适合作为完整论坛系统。

## 建议的第一阶段实现

第一阶段只做“全站讨论区”，不要先做“每篇文档评论”。

建议效果：

- 首页新增一个 `#discussion` 模块。
- 导航新增 `Discussion` / `讨论` 入口。
- 模块内嵌 giscus。
- 所有用户反馈和讨论统一进入 GitHub Discussions。

这样改动最小：

- 修改 `website/_templates/home.html`
- 修改 `website/_i18n/en.json`
- 修改 `website/_i18n/zh-CN.json`
- 修改 `website/styles.css`
- 修改 `scripts/build-site-i18n.ps1` 中导航和模板 token

不需要调整 GitHub Pages 工作流，也不需要新增运行时服务。

## 暂不建议每篇文档独立评论

当前文档主要是 Markdown 文件，发布工作流把 `docs/` 复制到 Pages 产物中。它们没有统一经过 HTML 文档模板渲染。

如果要给每篇文档都加独立评论，需要先处理：

- Markdown 到 HTML 的统一渲染。
- 文档页模板。
- 每篇文档的 giscus 页面映射策略。
- 多语言文档与评论串的对应关系。

这会把任务从“首页静态模块”扩大成“文档站生成系统改造”。当前阶段不建议先做。

## 实施步骤

### 1. GitHub 仓库开启 Discussions

进入 GitHub 仓库设置，开启 Discussions。

建议新增一个分类，例如：

- `General`
- `Q&A`
- `Feedback`

第一阶段可以使用 `General` 或 `Q&A`。

### 2. 安装 giscus GitHub App

打开：

<https://github.com/apps/giscus>

给 `Yetmos/Eva-CLI` 仓库授权。

### 3. 生成 giscus 配置

打开：

<https://giscus.app/>

填写：

- Repository: `Yetmos/Eva-CLI`
- Page mapping: `pathname`
- Discussion category: 建议选 `General` 或 `Q&A`
- Features: 可开启 reactions
- Theme: `preferred_color_scheme`
- Language: 中文页使用 `zh-CN`，英文页使用 `en`

页面会生成一段 script。关键字段包括：

```html
<script src="https://giscus.app/client.js"
  data-repo="Yetmos/Eva-CLI"
  data-repo-id="..."
  data-category="General"
  data-category-id="..."
  data-mapping="pathname"
  data-reactions-enabled="1"
  data-emit-metadata="0"
  data-input-position="top"
  data-theme="preferred_color_scheme"
  data-lang="en"
  crossorigin="anonymous"
  async>
</script>
```

`data-repo-id` 和 `data-category-id` 必须以 giscus 页面生成结果为准。

### 4. 修改首页模板

在 `website/_templates/home.html` 中，建议放在 feedback 模块前或后：

```html
<section class="discussion-section" id="discussion" aria-labelledby="discussion-title">
  <div class="section-heading">
    <div>
      <p class="eyebrow">{{discussionEyebrow}}</p>
      <h2 id="discussion-title">{{discussionTitle}}</h2>
    </div>
  </div>
  <p class="section-copy">{{discussionBody}}</p>
  <div class="discussion-frame">
{{discussionEmbed}}
  </div>
</section>
```

### 5. 修改 i18n 文案

在 `website/_i18n/en.json` 增加：

```json
"discussion": {
  "eyebrow": "Discussion",
  "title": "Discuss Eva-CLI",
  "body": "Ask questions, share implementation ideas, or leave design feedback through GitHub Discussions."
}
```

在 `website/_i18n/zh-CN.json` 增加：

```json
"discussion": {
  "eyebrow": "讨论",
  "title": "讨论 Eva-CLI",
  "body": "通过 GitHub Discussions 提问、交流实现思路或留下设计反馈。"
}
```

导航中建议增加：

```json
"discussion": "Discussion"
```

中文：

```json
"discussion": "讨论"
```

### 6. 修改构建脚本

在 `scripts/build-site-i18n.ps1` 中：

- `New-NavLinks` 增加 `DiscussionHref` 参数。
- 导航 HTML 增加 discussion 链接。
- home token 增加：
  - `discussionEyebrow`
  - `discussionTitle`
  - `discussionBody`
  - `discussionEmbed`
- 根据 locale 输出不同 `data-lang`。

建议把 giscus script 封装成一个函数，例如：

```powershell
function New-DiscussionEmbed {
  param(
    [Parameter(Mandatory = $true)][string]$LocaleCode
  )

  $giscusLang = if ($LocaleCode -eq "zh-CN") { "zh-CN" } else { "en" }

  return @"
          <script src="https://giscus.app/client.js"
            data-repo="Yetmos/Eva-CLI"
            data-repo-id="REPLACE_WITH_REPO_ID"
            data-category="General"
            data-category-id="REPLACE_WITH_CATEGORY_ID"
            data-mapping="pathname"
            data-reactions-enabled="1"
            data-emit-metadata="0"
            data-input-position="top"
            data-theme="preferred_color_scheme"
            data-lang="$giscusLang"
            crossorigin="anonymous"
            async>
          </script>
"@
}
```

注意替换：

- `REPLACE_WITH_REPO_ID`
- `REPLACE_WITH_CATEGORY_ID`

这两个值从 giscus 配置页获取。

### 7. 修改样式

在 `website/styles.css` 中增加：

```css
.discussion-section {
  padding-top: 56px;
}

.discussion-frame {
  margin-top: 20px;
  padding: 24px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--surface);
  box-shadow: 0 14px 34px rgba(127, 29, 29, 0.08);
}
```

如果希望减少视觉重量，也可以不加边框，只保留 `margin-top`。

### 8. 本地验证

修改后运行：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-site-i18n.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\validate-i18n.ps1
```

再检查：

- `website/index.html` 是否生成 discussion 区块。
- `website/zh-CN/index.html` 是否生成中文 discussion 区块。
- 模板中没有残留 `{{token}}`。
- 导航锚点能跳转到 `#discussion`。

## 后续升级路径

如果后续讨论活跃，再考虑：

- 给文档站增加统一 HTML 渲染。
- 每篇文档独立接入 giscus。
- 根据语言拆分 discussion category。
- 增加“提问前请先阅读”的轻量提示。
- 把邮件反馈模块简化为 Discussions 入口。

## 当前建议

先实现首页全站讨论区。

这是当前项目状态下成本最低、维护最简单、最符合 GitHub Pages 静态托管能力边界的方案。
