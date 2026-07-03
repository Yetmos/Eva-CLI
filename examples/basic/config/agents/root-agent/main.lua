local root = {}

function root.on_event(event, ctx)
  return {
    status = "accepted",
    agent_id = "root-agent",
    topic = event and event.topic or nil,
    capability = "config.lint",
    capability_input = "examples/basic/config",
    note = "V1.0 basic event accepted by root-agent",
  }
end

return root
