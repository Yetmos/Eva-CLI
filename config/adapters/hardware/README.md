# Hardware Adapters / 硬件适配器

## 中文

硬件 Adapter 使用 `transport: hardware`，并声明设备总线、匹配规则、身份、协议、热插拔、driver 和设备级 limits。V1.10.1 起，`hardware.driver.kind` 支持 `simulated`、`usb`、`serial`、`ble`、`socket` 和 `vendor_sdk` 的 typed 配置预留；当前示例仍保持 `enabled: false` 且使用 simulator，不打开真实设备。全局硬件权限边界位于 `config/policies/hardware.yaml`。

## English

Hardware Adapters use `transport: hardware` and declare device bus, match rules, identity, protocol, hotplug behavior, driver settings, and device-level limits. V1.10.1 reserves typed driver kinds for simulated, USB, serial, BLE, socket, and vendor SDK drivers. Global hardware permission boundaries live in `config/policies/hardware.yaml`.
