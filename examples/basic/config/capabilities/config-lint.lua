local capability = {}

function capability.lint(input, ctx)
  return {
    valid = true,
    findings = {},
    config_dir = input and input.config_dir or "config",
  }
end

return capability
