-- 中文：agent-a12 叶子节点模块，向宿主暴露标准事件处理入口。
local agent = {}

-- 中文：确认事件已由 agent-a12 处理，并声明统一回复主题。
-- 当前占位处理器不消费事件内容或宿主上下文，因此不会产生外部副作用。
function agent.on_event(event, ctx)
  return {
    status = "handled",
    agent_id = "agent-a12",
    reply_topic = "/sys/route-a/route-aa/reply",
  }
end

return agent
