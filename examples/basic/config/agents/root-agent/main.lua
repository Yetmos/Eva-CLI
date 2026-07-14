-- 中文：基础示例的根 Agent 模块，演示日志、审计和 capability 调用的宿主绑定。
local root = {}

-- 中文：处理输入事件，并同步调用 `config.lint` 生成可观察的示例结果。
-- 日志和审计先记录接受动作，再通过宿主工具边界调用 capability；任一宿主调用失败会
-- 由 Lua 运行时按统一错误语义终止处理，避免返回“accepted”掩盖实际失败。
function root.on_event(event, ctx)
  ctx.host.log("info", "root-agent accepted " .. event.event_id)
  ctx.host.audit("root-agent requested config.lint")
  local lint = ctx.tools.call("config.lint", "examples/basic/config")

  return {
    status = "accepted",
    agent_id = "root-agent",
    topic = event and event.topic or nil,
    note = "V1.0 basic event accepted by root-agent; tool=" .. lint.status .. "; output=" .. (lint.output or ""),
  }
end

return root
