[CmdletBinding()]
# 用 contracts/cli-json 中的 fixture 对 Eva CLI 的进程退出码和指定 JSON stream 做递归子集校验。
# 校验器允许实际响应增加字段，但不允许删除或改变 fixture 声明的契约；任一失败通过 throw
# 产生非零脚本退出，适合 CI 门禁。脚本只读取 fixture 并执行 CLI，不改写契约文件。
param(
  # 可选已构建 eva 可执行文件；为空时通过 cargo run 启动当前工作区二进制。
  [string]$Eva,
  # 可选契约目录；为空时固定使用仓库 contracts/cli-json，随后解析为绝对路径。
  [string]$ContractRoot
)

$ErrorActionPreference = "Stop"

# 所有 <repo> 占位符都解析到脚本所在仓库根，而非调用者当前目录。
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if ([string]::IsNullOrWhiteSpace($ContractRoot)) {
  $ContractRoot = Join-Path $Root "contracts/cli-json"
}
$ContractRoot = (Resolve-Path $ContractRoot).Path

# 统一抛出带校验器前缀的终止错误；调用方可从 CI 日志快速区分失败来源。
function Fail {
  param([string]$Message)
  throw "[cli-json-contract] $Message"
}

# 将 fixture 命令参数中的唯一受支持占位符 <repo> 展开为绝对仓库路径。
# 不做 shell 拼接或求值，参数仍以独立 argv 元素传递，避免引号和注入语义变化。
function Convert-CommandArg {
  param([string]$Value)
  $Value.Replace("<repo>", $Root)
}

# PowerShell 5.1 的 ProcessStartInfo 没有 ArgumentList；此 fallback 保留独立 argv 边界。
function ConvertTo-NativeArgument {
  param([AllowEmptyString()][string]$Value)

  if ($Value.Length -gt 0 -and $Value -notmatch '[\s"]') {
    return $Value
  }

  $builder = New-Object System.Text.StringBuilder
  [void]$builder.Append('"')
  $backslashes = 0
  foreach ($character in $Value.ToCharArray()) {
    if ($character -eq '\') {
      $backslashes += 1
      continue
    }
    if ($character -eq '"') {
      if ($backslashes -gt 0) {
        [void]$builder.Append((('\' * ($backslashes * 2)) -join ''))
      }
      [void]$builder.Append('\"')
      $backslashes = 0
      continue
    }
    if ($backslashes -gt 0) {
      [void]$builder.Append((('\' * $backslashes) -join ''))
      $backslashes = 0
    }
    [void]$builder.Append($character)
  }
  if ($backslashes -gt 0) {
    [void]$builder.Append((('\' * ($backslashes * 2)) -join ''))
  }
  [void]$builder.Append('"')
  $builder.ToString()
}

# 以原始参数数组运行 Eva，并绕过 Windows PowerShell 对 native stderr 的 ErrorRecord 包装。
# 返回对象不解析 JSON，使调用方先独立验证进程退出码，再选择 stdout 或 stderr 做契约比较。
function Invoke-EvaJson {
  param([string[]]$CommandArgs)

  if ([string]::IsNullOrWhiteSpace($Eva)) {
    $executable = "cargo"
    $arguments = @("run", "--quiet", "--") + @($CommandArgs)
  } else {
    $executable = $Eva
    $arguments = @($CommandArgs)
  }

  $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
  $startInfo = New-Object System.Diagnostics.ProcessStartInfo
  $startInfo.FileName = $executable
  $startInfo.WorkingDirectory = $Root
  $startInfo.UseShellExecute = $false
  $startInfo.CreateNoWindow = $true
  $startInfo.RedirectStandardOutput = $true
  $startInfo.RedirectStandardError = $true
  $startInfo.StandardOutputEncoding = $utf8NoBom
  $startInfo.StandardErrorEncoding = $utf8NoBom
  if ($null -ne $startInfo.PSObject.Properties["ArgumentList"]) {
    foreach ($argument in $arguments) {
      [void]$startInfo.ArgumentList.Add($argument)
    }
  } else {
    $startInfo.Arguments = @($arguments | ForEach-Object { ConvertTo-NativeArgument $_ }) -join " "
  }

  $process = New-Object System.Diagnostics.Process
  $process.StartInfo = $startInfo
  try {
    [void]$process.Start()
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $process.WaitForExit()
    $exitCode = [int]$process.ExitCode
    $stdout = $stdoutTask.GetAwaiter().GetResult()
    $stderr = $stderrTask.GetAwaiter().GetResult()
  } finally {
    $process.Dispose()
  }

  [pscustomobject]@{
    ExitCode = $exitCode
    Stdout = $stdout
    Stderr = $stderr
  }
}

# 返回 ConvertFrom-Json 产生对象的用户字段，排除 PowerShell 方法等非 JSON 成员。
function Get-JsonProperties {
  param($Value)
  @($Value.PSObject.Properties | Where-Object { $_.MemberType -eq "NoteProperty" })
}

# 判断值是否为 JSON object 对应的 PSCustomObject；null 明确不是 object。
function Test-IsJsonObject {
  param($Value)
  $null -ne $Value -and $Value -is [pscustomobject]
}

# 判断值是否为 JSON array 对应的 System.Array；标量不会被宽松当作单元素数组。
function Test-IsJsonArray {
  param($Value)
  $null -ne $Value -and $Value -is [System.Array]
}

# 尝试执行一次递归子集比较并返回布尔结果。
# 该函数只供数组“任一实际元素匹配”搜索使用，故意吞掉候选不匹配异常；最终没有候选时
# 由外层 Assert-ContractSubset 生成带数组路径和期望元素的权威失败消息。
function Test-ContractMatch {
  param(
    $Actual,
    $Expected
  )
  try {
    Assert-ContractSubset -Actual $Actual -Expected $Expected -Path "$"
    $true
  } catch {
    $false
  }
}

# 递归断言 Actual 覆盖 Expected 声明的 JSON 子集。
# Path 使用 `$` 起始的属性路径定位失败；object 只检查期望字段，array 要求每个期望元素
# 至少匹配一个实际元素（不要求顺序或长度相等），标量和 null 则严格相等。
function Assert-ContractSubset {
  param(
    $Actual,
    $Expected,
    [string]$Path
  )

  if (Test-IsJsonObject $Expected) {
    $expectedProperties = Get-JsonProperties $Expected
    # `{"__contains":"text"}` 是 fixture 的字符串包含匹配器；只有单一该字段时才启用，
    # 避免真实业务对象恰好包含同名字段时被误解释。
    if ($expectedProperties.Count -eq 1 -and $expectedProperties[0].Name -eq "__contains") {
      if (-not ($Actual -is [string])) {
        Fail "$Path expected a string containing '$($expectedProperties[0].Value)'."
      }
      if (-not $Actual.Contains([string]$expectedProperties[0].Value)) {
        Fail "$Path did not contain '$($expectedProperties[0].Value)'."
      }
      return
    }

    if (-not (Test-IsJsonObject $Actual)) {
      Fail "$Path expected an object."
    }
    # 对象采用向前兼容子集语义：实际响应可新增字段，但 fixture 声明的字段必须递归匹配。
    foreach ($property in $expectedProperties) {
      $actualProperty = $Actual.PSObject.Properties[$property.Name]
      if ($null -eq $actualProperty) {
        Fail "$Path.$($property.Name) is missing."
      }
      Assert-ContractSubset `
        -Actual $actualProperty.Value `
        -Expected $property.Value `
        -Path "$Path.$($property.Name)"
    }
    return
  }

  if (Test-IsJsonArray $Expected) {
    if (-not (Test-IsJsonArray $Actual)) {
      Fail "$Path expected an array."
    }
    # 数组采用无序包含语义，便于 fixture 锁定关键成员而不固化非契约顺序和额外条目。
    foreach ($expectedItem in $Expected) {
      $matched = $false
      foreach ($actualItem in $Actual) {
        if (Test-ContractMatch -Actual $actualItem -Expected $expectedItem) {
          $matched = $true
          break
        }
      }
      if (-not $matched) {
        $itemJson = $expectedItem | ConvertTo-Json -Depth 100 -Compress
        Fail "$Path does not contain required item $itemJson."
      }
    }
    return
  }

  if ($null -eq $Expected) {
    if ($null -ne $Actual) {
      Fail "$Path expected null."
    }
    return
  }

  if ($Actual -ne $Expected) {
    Fail "$Path expected '$Expected' but found '$Actual'."
  }
}

# 固定按文件名排序执行 fixture，使失败顺序和 CI 日志可重复。
$contractFiles = @(Get-ChildItem -LiteralPath $ContractRoot -Filter "*.json" | Sort-Object Name)
if ($contractFiles.Count -eq 0) {
  Fail "No contract fixture JSON files found under $ContractRoot."
}

$validated = 0
# 每个 fixture 依次验证元数据、退出码、指定 stream JSON 和递归 expected 子集；任一阶段失败立即终止。
foreach ($file in $contractFiles) {
  $fixture = Get-Content -LiteralPath $file.FullName -Raw -Encoding utf8 | ConvertFrom-Json
  if ([string]::IsNullOrWhiteSpace($fixture.id)) {
    Fail "$($file.Name) must declare id."
  }
  if (-not (Test-IsJsonArray $fixture.command)) {
    Fail "$($file.Name) must declare command as an array."
  }
  $args = @($fixture.command | ForEach-Object { Convert-CommandArg ([string]$_) })
  $jsonStream = if ([string]::IsNullOrWhiteSpace($fixture.json_stream)) {
    "stdout"
  } else {
    ([string]$fixture.json_stream).ToLowerInvariant()
  }
  if ($jsonStream -notin @("stdout", "stderr")) {
    Fail "$($fixture.id) json_stream must be stdout or stderr."
  }
  # 未声明进程退出码的契约默认要求成功；显式失败 fixture 可覆盖为对应稳定退出码。
  $expectedExitCode = if ($null -eq $fixture.process_exit_code) { 0 } else { [int]$fixture.process_exit_code }
  $result = Invoke-EvaJson -CommandArgs $args
  if ($result.ExitCode -ne $expectedExitCode) {
    Fail "$($fixture.id) process exit code was $($result.ExitCode), expected $expectedExitCode."
  }
  switch ($jsonStream) {
    "stdout" { $jsonText = $result.Stdout }
    "stderr" { $jsonText = $result.Stderr }
  }
  try {
    $actualJson = $jsonText | ConvertFrom-Json
  } catch {
    Fail "$($fixture.id) did not emit valid JSON on ${jsonStream}: $jsonText"
  }
  if ($null -ne $actualJson.PSObject.Properties["exit_code"] -and [int]$actualJson.exit_code -ne $result.ExitCode) {
    Fail "$($fixture.id) JSON exit_code was $($actualJson.exit_code), process exit code was $($result.ExitCode)."
  }
  Assert-ContractSubset -Actual $actualJson -Expected $fixture.expected -Path "$($fixture.id)"
  $validated += 1
}

Write-Host "CLI JSON contracts validated: $validated fixture(s)."
