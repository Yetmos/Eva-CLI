# eva-policy/src / 策略源码

## 中文

这里保存权限合并、权限收紧和沙箱策略逻辑。任何策略计算只能收紧权限，不能替 Agent、Adapter 或请求放宽权限边界。

## English

This directory contains permission merging, permission narrowing, and sandbox policy logic. Policy evaluation may only narrow permissions; it must not expand boundaries for Agents, Adapters, or requests.
