# eva-scheduler/src / 调度源码

## 中文

这里保存 Topic matcher、订阅表、路由规则和 mailbox 投递。它只做事件投递，不执行 Lua，不调用 Adapter，不承载业务判断。

## English

This directory contains Topic matching, subscription tables, routing rules, and mailbox delivery. It delivers events only; it does not execute Lua, invoke Adapters, or own business decisions.
