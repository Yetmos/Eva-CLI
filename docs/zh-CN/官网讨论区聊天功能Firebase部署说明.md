# 官网讨论区聊天功能 Firebase 部署说明

更新日期：2026-06-27

本文对应 `官网讨论区聊天功能Firebase接入方案.md` 的当前实现。官网仍由 GitHub Pages
托管，Firebase 只承担 Auth、Firestore、Storage、Functions、FCM 和 RTDB。

## 1. Firebase Console 准备

在项目 `eva-cli-7ad30` 中确认：

1. Authentication 启用 GitHub provider。
2. GitHub OAuth callback URL 使用：
   `https://eva-cli-7ad30.firebaseapp.com/__/auth/handler`
3. Authentication authorized domains 至少包含：
   - `www.eva-cli.com`
   - `eva-cli.com`
   - `eva-cli-7ad30.firebaseapp.com`
   - `localhost`
4. 已创建 Firestore database、Cloud Storage bucket、Realtime Database。
5. Cloud Messaging 已生成 Web Push VAPID public key。
6. Cloud Functions 使用 Node.js 20，项目需要可部署 Functions 的计费/权限配置。

## 2. 本地部署命令

先登录并选择项目：

```sh
firebase login
firebase use eva-cli-7ad30
```

安装 Functions 依赖：

```sh
cd functions
npm install
npm run lint
cd ..
```

部署 Firebase 后端：

```sh
firebase deploy --only functions,firestore:rules,firestore:indexes,storage,database
```

官网仍通过现有 GitHub Pages workflow 发布。推送 `website/**`、`docs/**`、`assets/**`
或 `.github/workflows/pages.yml` 后，GitHub Actions 会运行 `scripts/build-site-i18n.ps1`
并部署静态站点。

## 3. 当前实现范围

已接入：

- GitHub 登录入口和首次 profile 创建。
- 官网公共讨论区加入入口。
- 单聊创建。
- 群聊创建。
- 群邀请链接创建和接受。
- 文本消息。
- 图片上传到 Cloud Storage 后写入消息。
- 会话列表投影。
- 群匿名显示名开关；真实 UID 写入成员不可读的 `messageAudits` 审计集合。
- FCM Web token 注册和后台通知 service worker。
- 账号注销 callable，删除用户 profile、sessions、成员关系、私聊 thread、用户消息和媒体。
- Firestore、Storage、Realtime Database 基础 Rules。

## 4. 上线前检查

1. 在 Firebase Console 确认 GitHub provider 的 Client Secret 只保存在 Console，不进入仓库。
2. 确认 `tmp/` 没有被提交；它包含 service account 和临时配置记录。
3. 用两个 GitHub 账号分别验证登录、加入讨论区、发消息、创建单聊和群聊。
4. 在 HTTPS 域名 `https://www.eva-cli.com/` 验证 FCM 通知权限和 service worker。
5. 对 Firestore/Storage Rules 跑 emulator rules tests 后再强制 App Check。

## 5. 已知后续加固

- 当前 App Check 没有强制启用，等生产 Web app 注册并验证后再打开 `enforceAppCheck`。
- 昵称敏感词、消息审核、图片缩略图和 sticker catalog 仍是后续内容安全工作。
- 大群 fanout 目前按成员逐个写 session，初期应限制群规模。
- 账号注销是直接 callable 执行，数据量变大后应改为后台任务队列。
