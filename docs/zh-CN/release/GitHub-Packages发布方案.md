# Eva-CLI GitHub Packages 发布方案

日期：2026-07-06
范围：通过 GitHub Container Registry 进行 GitHub Packages 交付

Eva-CLI 使用 GitHub Container Registry（GHCR）发布 GitHub Packages 容器镜像。
该通道补充 GitHub Release，不替代 release tag、GitHub Release 记录或 release
evidence artifact。

包名为：

```text
ghcr.io/yetmos/eva-cli
```

容器入口是 `eva` CLI 二进制。

## 通道契约

GitHub Packages 通道在 `.github/workflows/release.yml` 中实现。它位于 Windows、
macOS、Linux 三平台 release verification matrix 之后，GitHub Release 正文创建或更新之前。

package job 会：

- checkout 不可变 release tag；
- 对该 tag 运行版本管理校验；
- 构建本地容器镜像，并用 `eva --version` 做 package smoke test；
- 向 GHCR 发布 `linux/amd64` 和 `linux/arm64` 多平台镜像；
- 在 `release-evidence/package-ghcr.json` 中记录 package 名称、registry URL、source tag、source SHA、digest、tags 和 platforms；
- 上传 `package-evidence-${RELEASE_TAG}`，再由 publish job 合并进 `release-evidence-${RELEASE_TAG}`。

## Tag 规则

所有 package tag 都从驱动 GitHub Release workflow 的同一个 release tag 推导。

| Release 类型 | Git tag 示例 | GHCR tags |
| --- | --- | --- |
| alpha | `v1.5.1-alpha` | `1.5.1-alpha`、`sha-<short>` |
| beta | `v1.5.1-beta.1` | `1.5.1-beta.1`、`sha-<short>` |
| stable | `v1.5.1` | `1.5.1`、`1.5`、`latest`、`sha-<short>` |

只有 stable release 可以更新 `latest`。alpha 和 beta release 不得发布或移动 `latest`。

## 权限

package job 使用仓库 `GITHUB_TOKEN` 和以下 job 权限：

```yaml
permissions:
  contents: read
  packages: write
```

默认路径不需要 personal access token。只有未来 package 流程需要跨仓库访问私有 package，
且 `GITHUB_TOKEN` 无法满足时，才使用最小权限 PAT。

## 拉取验证

release workflow 成功后，用以下命令验证 package：

```powershell
docker pull ghcr.io/yetmos/eva-cli:<version>
docker run --rm ghcr.io/yetmos/eva-cli:<version> --version
```

正式 release 还需要验证：

```powershell
docker pull ghcr.io/yetmos/eva-cli:latest
```

Docker 或 GHCR package 页面返回的 digest 必须与
`release-evidence/package-ghcr.json` 一致。

## 范围限制

GitHub Packages 不是 Cargo crate registry 替代品。Rust crate 公开发布仍应作为
crates.io 决策单独评估。

该 GHCR 通道不新增签名安装器、系统包管理器 package 或 provenance bundle。这些仍属于后续 release 范围。

现有 `v1.5.0` release tag 早于该 package 通道，且必须保持不可变。不要仅为了回填
GHCR 而移动或重新发布该 tag。package 发布适用于后续包含本 workflow 和 Dockerfile 支持的 release tag。
