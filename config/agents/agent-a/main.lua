-- 中文：route-a 节点的 Agent 模块；运行时通过返回的表发现标准事件回调。
local route_a = {}

-- 中文：处理路由事件并声明下一跳目标。
-- `event` 和 `ctx` 由宿主统一传入；当前占位实现不读取它们，只返回稳定的拓扑数据，
-- 供运行时验证从 agent-a 到两个叶子 Agent 的扇出路径。
function route_a.on_event(event, ctx)
  return {
    status = "routed",
    agent_id = "agent-a",
    topic = "/sys/route-a/route-aa",
    targets = { "agent-a11", "agent-a12" },
  }
end

return route_a
