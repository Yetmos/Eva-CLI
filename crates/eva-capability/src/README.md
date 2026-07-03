# eva-capability/src

更新时间：2026-07-03

| 文件 | V0.4 状态 | 说明 |
| --- | --- | --- |
| `registry.rs` | 已实现 | `CapabilityDescriptor`、`CapabilityRegistry`、V0.4 builtin registry。 |
| `router.rs` | 已实现 | `CapabilityRouter`，执行 `config.lint` 和 `runtime.echo` builtin。 |
| `host_api.rs` | 已实现 | `CapabilityHostApi` trait。 |
| `generation.rs` | 已实现边界 | `CapabilityGeneration` marker。 |
| `lib.rs` | 已实现 | re-export V0.4 公开类型。 |

验证：`cargo test -p eva-capability`。
