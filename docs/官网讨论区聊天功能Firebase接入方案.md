# 官网讨论区聊天功能 Firebase 接入方案

> 迁移兼容入口：完整中文方案见
> [zh-CN/官网讨论区聊天功能Firebase接入方案.md](zh-CN/官网讨论区聊天功能Firebase接入方案.md)。
> 英文 canonical source 见
> [en/discussion-chat-firebase-plan.md](en/discussion-chat-firebase-plan.md)。

## 摘要

该方案使用 Firebase Authentication、Cloud Firestore、Cloud Storage、
Cloud Functions、Security Rules、App Check、Firebase Cloud Messaging 和 Realtime Database，为官网讨论区提供
GitHub 登录、单聊、群聊、图片/文本/表情包、会话列表、群匿名模式、邀请链接加群和
账号注销能力。FCM 只用于离线/后台通知，消息内容和顺序仍以 Firestore 为准。

![官网讨论区聊天 Firebase 架构](assets/discussion-chat-firebase-architecture.zh-CN.svg)

| 能力 | 方案 |
| --- | --- |
| 登录 | Firebase Auth GitHub provider，首次登录通过 Cloud Function 分配随机中文名。 |
| 消息 | Firestore 保存 threads/members/messages，Storage 保存图片和表情包。 |
| 会话列表 | `users/{uid}/sessions/{threadId}` 维护用户侧投影。 |
| FCM 通知 | 每个用户/设备登记 Web Push token，由 Cloud Functions 在消息写入后发送通知。 |
| 群匿名 | 每个群成员单独保存匿名名，普通成员看匿名名，后台仍可审计。 |
| 分享加群 | 分享随机 token，数据库只保存 token hash，由 Cloud Function 验证并入群。 |
| 账号注销 | Cloud Function 统一删除 Auth、Firestore、Storage、RTDB 中的用户数据。 |

更多数据模型、函数清单、Rules 边界、测试计划和风险缓解见完整中文方案。
