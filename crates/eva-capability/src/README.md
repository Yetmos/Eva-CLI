# eva-capability/src

更新时间：2026-07-07

| 文件 | V0.4 状态 | 说明 |
| --- | --- | --- |
| `registry.rs` | 已实现 | `CapabilityDescriptor`、`CapabilityRegistry`、V0.4 builtin registry；descriptor 保留 manifest provider selection metadata。 |
| `router.rs` | 已实现 | `CapabilityRouter`，执行 `config.lint` 和 `runtime.echo` builtin，并可为 adapter-backed capability 生成 provider plan。 |
| `selection.rs` | 已实现 V1.8.5.1 | `CapabilityProviderSelection`、`CapabilityProviderPlan` 和 provider source，负责 explicit/default/fallback 稳定排序与去重。 |
| `host_api.rs` | 已实现 | `CapabilityHostApi` trait。 |
| `generation.rs` | 已实现边界 | `CapabilityGeneration` marker。 |
| `lib.rs` | 已实现 | re-export capability 公开类型。 |

验证：`cargo test -p eva-capability`。
