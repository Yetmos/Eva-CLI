# 备份、迁移包与 Release Snapshot 架构方案

本文档已纳入多语言文档结构：

- 中文正文：[zh-CN/备份迁移包与ReleaseSnapshot架构方案.md](zh-CN/备份迁移包与ReleaseSnapshot架构方案.md)
- English default entry：[en/backup-migration-release-snapshot.md](en/backup-migration-release-snapshot.md)

核心结论：备份、迁移包和 release snapshot 的可信执行应由 Runtime 实现；
Agent 只负责编排、解释、请求和总结，不应成为 scope、policy、verification、
mutation、rollback 或 audit 的事实来源。
