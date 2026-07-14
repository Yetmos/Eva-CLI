-- 中文：根 Agent 模块；返回表中的回调由 Eva Lua 宿主按约定加载。
local root = {}

-- 中文：接受根主题事件并返回最小处理摘要。
-- 对可能缺失的事件使用短路取值，避免占位配置在探测调用中因空事件崩溃；实际分发
-- 仍由运行时绑定负责，本函数不直接选择下游 Agent。
function root.on_event(event, ctx)
  return {
    status = "accepted",
    agent_id = "root-agent",
    topic = event and event.topic or nil,
    note = "Root agent placeholder handler. Runtime bindings will own dispatch.",
  }
end

return root
