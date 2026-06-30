local root = {}

function root.on_event(event, ctx)
  return {
    status = "accepted",
    agent_id = "root-agent",
    topic = event and event.topic or nil,
    note = "Root agent placeholder handler. Runtime bindings will own dispatch.",
  }
end

return root
