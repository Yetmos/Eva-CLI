local agent = {}

function agent.on_event(event, ctx)
  return {
    status = "handled",
    agent_id = "agent-a12",
    reply_topic = "/sys/route-a/route-aa/reply",
  }
end

return agent
