local route_a = {}

function route_a.on_event(event, ctx)
  return {
    status = "routed",
    agent_id = "agent-a",
    topic = "/sys/route-a/route-aa",
    targets = { "agent-a11", "agent-a12" },
  }
end

return route_a
