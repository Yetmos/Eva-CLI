# eva-runtime/src / 运行时源码

## 中文

这里保存运行时组合根、服务装配、启动关闭和 generation 内服务句柄。具体业务契约仍应下沉到对应 crate，避免下层反向依赖 `eva-runtime`。

## English

This directory contains the runtime composition root, service wiring, startup/shutdown, and generation service handles. Domain contracts should stay in their owning crates so lower crates do not depend back on `eva-runtime`.
