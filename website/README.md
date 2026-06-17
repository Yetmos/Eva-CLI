# Website

这里维护 Eva-CLI 官网源码。

官网保持零运行时依赖。页面由构建期 i18n 脚本从模板和 locale JSON 生成：

- `_templates/home.html`
- `_templates/docs-index.html`
- `_i18n/en.json`
- `_i18n/zh-CN.json`
- `index.html`
- `zh-CN/index.html`
- `docs/index.html`
- `styles.css`

GitHub Pages 工作流会把 `website/`、`docs/` 和 `assets/` 组合成可发布站点。官网页面中的 `docs/` 与 `assets/` 链接按发布后的站点根目录解析。

带文字的内容型图片同样按语言发布。新增或修改这类图片时，在 `docs/_i18n/manifest.json` 的 `assets` 区域登记英文 source、各语言 translation 和状态；构建脚本会为不同语言首页生成对应图片 URL。Logo 等品牌中性资源保持共用。

本地生成与校验：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File ..\scripts\build-site-i18n.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File ..\scripts\validate-i18n.ps1
```

发布产物会删除 `_templates/` 和 `_i18n/`，公开站点只保留静态 HTML、Markdown 和资源文件。
