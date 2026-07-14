-- 中文：基础示例使用的配置检查 capability 模块。
local capability = {}

-- 中文：返回可序列化的检查结果，缺省目录与主项目 capability 保持一致。
-- 该示例只验证工具调用链路，不读取或修改配置文件。
function capability.lint(input, ctx)
  return {
    valid = true,
    findings = {},
    config_dir = input and input.config_dir or "config",
  }
end

return capability
