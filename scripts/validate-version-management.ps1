[CmdletBinding()]
# 校验 Cargo 版本、可选 release tag、面向用户版本文本、i18n 文档权威关系和发布工作流接线。
# 脚本只读；任何不一致通过 Fail 抛出异常并产生非零退出，防止 tag、二进制、文档或 GHCR
# 发布元数据指向不同版本。成功输出规范化后的 cargo/human/status/tag 事实。
param(
  # CI 可显式传入 tag；缺省读取 RELEASE_TAG。空值表示仅校验仓库内部版本，不校验 tag。
  [string]$Tag = $env:RELEASE_TAG
)

$ErrorActionPreference = "Stop"

# 文件读取始终锚定脚本所在仓库根，不依赖调用者当前目录。
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
# 仅允许无前导零 SemVer，以及 alpha/beta 和可选正整数序号；release tag 在此模式前追加 v。
$VersionPattern = "^(?<major>0|[1-9]\d*)\.(?<minor>0|[1-9]\d*)\.(?<patch>0|[1-9]\d*)(?:-(?<status>alpha|beta)(?:\.(?<serial>[1-9]\d*))?)?$"

# 统一抛出带 version-management 前缀的终止错误，供 CI 准确归因。
function Fail {
  param([string]$Message)
  throw "[version-management] $Message"
}

# 以 UTF-8 读取必需仓库文件；缺失与读取失败都终止验证，不允许空内容回退。
function Read-RepoFile {
  param([string]$RelativePath)
  $path = Join-Path $Root $RelativePath
  if (-not (Test-Path -LiteralPath $path)) {
    Fail "Missing required file: $RelativePath"
  }
  Get-Content -LiteralPath $path -Raw -Encoding utf8
}

# 断言指定文件包含精确文本片段。
# 使用 String.Contains 而非正则，确保工作流命令、镜像名和文档链接按字面量锁定。
function Assert-Contains {
  param(
    [string]$RelativePath,
    [string]$Needle
  )
  $text = Read-RepoFile $RelativePath
  if (-not $text.Contains($Needle)) {
    Fail "$RelativePath must contain '$Needle'."
  }
}

# 第一阶段从根 Cargo.toml 提取 package 与 workspace package 版本，并要求两者完全一致。
$cargoText = Read-RepoFile "Cargo.toml"
$versionMatches = [regex]::Matches($cargoText, '(?m)^version\s*=\s*"([^"]+)"')
if ($versionMatches.Count -lt 2) {
  Fail "Cargo.toml must define both package.version and workspace.package.version."
}

$packageVersion = $versionMatches[0].Groups[1].Value
$workspaceVersion = $versionMatches[$versionMatches.Count - 1].Groups[1].Value
if ($packageVersion -ne $workspaceVersion) {
  Fail "package.version '$packageVersion' must match workspace.package.version '$workspaceVersion'."
}

$versionMatch = [regex]::Match($packageVersion, $VersionPattern)
if (-not $versionMatch.Success) {
  Fail "Cargo version '$packageVersion' must be stable SemVer or alpha/beta prerelease SemVer."
}

# 从同一正则匹配派生所有下游表示，避免各验证分支独立解释 prerelease。
$status = if ($versionMatch.Groups["status"].Success) {
  $versionMatch.Groups["status"].Value
} else {
  "release"
}
$humanVersion = if ($status -eq "release") {
  "V$packageVersion-release"
} else {
  "V$packageVersion"
}
$expectedTag = "v$packageVersion"

# 第二阶段仅在调用方提供 tag 时校验：格式必须带 v，且规范值必须与 Cargo 版本逐字匹配。
if (-not [string]::IsNullOrWhiteSpace($Tag)) {
  $normalizedTag = $Tag.Trim()
  $tagPattern = "^v$($VersionPattern.TrimStart('^').TrimEnd('$'))$"
  if (-not ([regex]::Match($normalizedTag, $tagPattern).Success)) {
    Fail "Release tag '$normalizedTag' must be vMAJOR.MINOR.PATCH, vMAJOR.MINOR.PATCH-alpha, or vMAJOR.MINOR.PATCH-beta.N."
  }
  if ($normalizedTag -ne $expectedTag) {
    Fail "Release tag '$normalizedTag' must match Cargo version '$packageVersion' as '$expectedTag'."
  }
}

# 第三阶段验证 i18n manifest 中 README、版本方案和包发布文档的路径及内容权威语言。
$manifest = Read-RepoFile "docs/_i18n/manifest.json" | ConvertFrom-Json
$readmeDoc = @($manifest.documents | Where-Object { $_.id -eq "readme" }) | Select-Object -First 1
if ($null -eq $readmeDoc) {
  Fail "docs/_i18n/manifest.json must register document id 'readme'."
}
$docsZhEntryPath = $readmeDoc.translations.'zh-CN'
if ([string]::IsNullOrWhiteSpace($docsZhEntryPath) -or -not $docsZhEntryPath.StartsWith("docs/zh-CN/")) {
  Fail "readme zh-CN path must live under docs/zh-CN/."
}
if (-not (Test-Path -LiteralPath (Join-Path $Root $docsZhEntryPath))) {
  Fail "readme zh-CN file does not exist: $docsZhEntryPath"
}

# 版本管理与包发布的中文文档目前是详细内容权威源；英文 source 仍必须位于稳定发布目录。
$versionDoc = @($manifest.documents | Where-Object { $_.id -eq "version-management-plan" }) | Select-Object -First 1
if ($null -eq $versionDoc) {
  Fail "docs/_i18n/manifest.json must register document id 'version-management-plan'."
}
if ($versionDoc.source -ne "docs/en/release/version-management-plan.md") {
  Fail "version-management-plan source path must be docs/en/release/version-management-plan.md."
}
$versionPlanZhPath = $versionDoc.translations.'zh-CN'
if ([string]::IsNullOrWhiteSpace($versionPlanZhPath) -or -not $versionPlanZhPath.StartsWith("docs/zh-CN/release/")) {
  Fail "version-management-plan zh-CN path must live under docs/zh-CN/release/."
}
if (-not (Test-Path -LiteralPath (Join-Path $Root $versionPlanZhPath))) {
  Fail "version-management-plan zh-CN file does not exist: $versionPlanZhPath"
}
if ($versionDoc.contentAuthority.locale -ne "zh-CN") {
  Fail "version-management-plan content authority must remain zh-CN while Chinese is the detailed source."
}

$packageDoc = @($manifest.documents | Where-Object { $_.id -eq "github-packages-publishing" }) | Select-Object -First 1
if ($null -eq $packageDoc) {
  Fail "docs/_i18n/manifest.json must register document id 'github-packages-publishing'."
}
if ($packageDoc.source -ne "docs/en/release/github-packages-publishing.md") {
  Fail "github-packages-publishing source path must be docs/en/release/github-packages-publishing.md."
}
$packagePlanZhPath = $packageDoc.translations.'zh-CN'
if ([string]::IsNullOrWhiteSpace($packagePlanZhPath) -or -not $packagePlanZhPath.StartsWith("docs/zh-CN/release/")) {
  Fail "github-packages-publishing zh-CN path must live under docs/zh-CN/release/."
}
if (-not (Test-Path -LiteralPath (Join-Path $Root $packagePlanZhPath))) {
  Fail "github-packages-publishing zh-CN file does not exist: $packagePlanZhPath"
}
if ($packageDoc.contentAuthority.locale -ne "zh-CN") {
  Fail "github-packages-publishing content authority must remain zh-CN while Chinese is the detailed source."
}

# 第四阶段锁定 CI/release/GHCR/Dockerfile 与双语文档之间的关键接线，防止功能存在但未被门禁或文档引用。
Assert-Contains $versionPlanZhPath "scripts/validate-version-management.ps1"
Assert-Contains "docs/en/release/version-management-plan.md" "scripts/validate-version-management.ps1"
Assert-Contains ".github/workflows/ci.yml" "validate-version-management.ps1"
Assert-Contains ".github/workflows/release.yml" "validate-version-management.ps1"
Assert-Contains ".github/workflows/release.yml" "packages: write"
Assert-Contains ".github/workflows/release.yml" "ghcr.io/yetmos/eva-cli"
Assert-Contains ".github/workflows/release.yml" "docker/build-push-action@v6"
Assert-Contains ".github/workflows/release.yml" "package-ghcr.json"
Assert-Contains "Dockerfile" "cargo build --release --locked --bin eva"
Assert-Contains "Dockerfile" 'ENTRYPOINT ["eva"]'
Assert-Contains ".dockerignore" "target"
Assert-Contains $packagePlanZhPath "ghcr.io/yetmos/eva-cli"
Assert-Contains "docs/en/release/github-packages-publishing.md" "ghcr.io/yetmos/eva-cli"
Assert-Contains "docs/en/README.md" "release/github-packages-publishing.md"
$packageReadmeLink = $packagePlanZhPath.Substring("docs/zh-CN/".Length)
Assert-Contains $docsZhEntryPath $packageReadmeLink

# 所有面向用户入口必须展示同一个派生 humanVersion；CLI 源码也包含该历史发布标签。
$humanVersionFiles = @(
  "README.md",
  "README.zh-CN.md",
  "docs/en/README.md",
  $docsZhEntryPath,
  "crates/eva-cli/src/run.rs"
)

foreach ($relativePath in $humanVersionFiles) {
  Assert-Contains $relativePath $humanVersion
}

Assert-Contains "crates/eva-cli/src/run.rs" "const RELEASE_STATUS: &str = `"$status`";"
Assert-Contains "docs/en/README.md" "release/version-management-plan.md"
$zhReadmeLink = $versionPlanZhPath.Substring("docs/zh-CN/".Length)
Assert-Contains $docsZhEntryPath $zhReadmeLink

Write-Host "Version management validated: cargo=$packageVersion human=$humanVersion status=$status tag=$expectedTag"
