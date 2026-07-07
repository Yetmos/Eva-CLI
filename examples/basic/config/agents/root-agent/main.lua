local root = {}

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
