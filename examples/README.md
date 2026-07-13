# Examples

本目录只维护仓库中实际存在且可运行的示例。

| 示例 | 说明 |
| --- | --- |
| `basic/` | 最小事件闭环，覆盖 task 状态/日志/取消、timeout 和 dead-letter replay 诊断。 |

```powershell
cargo run -- run --example basic --output json
cargo run -- task status --output json
```
