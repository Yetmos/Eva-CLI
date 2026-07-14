-- 中文：配置检查 capability 模块；宿主通过返回表发现 `lint` 入口。
local capability = {}

-- 中文：返回配置检查的稳定结构化结果。
-- 输入未提供 `config_dir` 时使用项目默认目录；当前实现是连通性占位，不执行文件写入。
function capability.lint(input, ctx)
  return {
    valid = true,
    findings = {},
    config_dir = input and input.config_dir or "config",
  }
end

return capability
