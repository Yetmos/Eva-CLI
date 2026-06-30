# eva-core / 基础契约

## 中文

`eva-core` 保存跨模块稳定数据契约，例如 `Event`、`Topic`、ID、capability 名称、invoke request/response 和结构化错误。它不负责 Tokio 任务装配、文件系统访问或 provider 私有逻辑。

## English

`eva-core` owns stable cross-module data contracts such as `Event`, `Topic`, IDs, capability names, invoke request/response, and structured errors. It does not own Tokio task wiring, filesystem access, or provider-specific logic.
