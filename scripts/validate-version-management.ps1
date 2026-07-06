[CmdletBinding()]
param(
  [string]$Tag = $env:RELEASE_TAG
)

$ErrorActionPreference = "Stop"

$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$VersionPattern = "^(?<major>0|[1-9]\d*)\.(?<minor>0|[1-9]\d*)\.(?<patch>0|[1-9]\d*)(?:-(?<status>alpha|beta)(?:\.(?<serial>[1-9]\d*))?)?$"

function Fail {
  param([string]$Message)
  throw "[version-management] $Message"
}

function Read-RepoFile {
  param([string]$RelativePath)
  $path = Join-Path $Root $RelativePath
  if (-not (Test-Path -LiteralPath $path)) {
    Fail "Missing required file: $RelativePath"
  }
  Get-Content -LiteralPath $path -Raw -Encoding utf8
}

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

$manifest = Read-RepoFile "docs/_i18n/manifest.json" | ConvertFrom-Json
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
Assert-Contains "docs/zh-CN/中文文档入口.md" $packageReadmeLink

$humanVersionFiles = @(
  "README.md",
  "README.zh-CN.md",
  "docs/en/README.md",
  "docs/zh-CN/中文文档入口.md",
  "crates/eva-cli/src/run.rs"
)

foreach ($relativePath in $humanVersionFiles) {
  Assert-Contains $relativePath $humanVersion
}

Assert-Contains "crates/eva-cli/src/run.rs" "const RELEASE_STATUS: &str = `"$status`";"
Assert-Contains "docs/en/README.md" "release/version-management-plan.md"
$zhReadmeLink = $versionPlanZhPath.Substring("docs/zh-CN/".Length)
Assert-Contains "docs/zh-CN/中文文档入口.md" $zhReadmeLink

Write-Host "Version management validated: cargo=$packageVersion human=$humanVersion status=$status tag=$expectedTag"
