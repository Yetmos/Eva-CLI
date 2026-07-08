# Eva-CLI Install, Upgrade, And Uninstall

Date: 2026-07-08
Scope: V1.11.2 native archive install smoke and package-manager dry-run evidence

Eva-CLI V1.11.2 publishes native archive metadata for Windows, Linux, and
macOS in the release workflow. The workflow verifies each archive by extracting
it into a temporary install directory and running `eva --version` from that
installed location. It also verifies the GHCR package metadata with a dry-run
inspection before the GitHub Release evidence is uploaded.

## Windows Archive

Install:

```powershell
Expand-Archive -LiteralPath eva-cli-<version>-x86_64-pc-windows-msvc.zip -DestinationPath eva-cli
.\eva-cli\eva.exe --version
```

Upgrade:

```powershell
Remove-Item -LiteralPath eva-cli -Recurse -Force
Expand-Archive -LiteralPath eva-cli-<new-version>-x86_64-pc-windows-msvc.zip -DestinationPath eva-cli
.\eva-cli\eva.exe upgrade check --output json
```

Uninstall:

```powershell
Remove-Item -LiteralPath eva-cli -Recurse -Force
```

## Linux Archive

Install:

```bash
mkdir -p eva-cli
tar -C eva-cli -xzf eva-cli-<version>-x86_64-unknown-linux-gnu.tar.gz
./eva-cli/eva --version
```

Upgrade:

```bash
rm -rf eva-cli
mkdir -p eva-cli
tar -C eva-cli -xzf eva-cli-<new-version>-x86_64-unknown-linux-gnu.tar.gz
./eva-cli/eva upgrade check --output json
```

Uninstall:

```bash
rm -rf eva-cli
```

## macOS Archive

Install:

```bash
mkdir -p eva-cli
tar -C eva-cli -xzf eva-cli-<version>-<target>.tar.gz
./eva-cli/eva --version
```

Upgrade:

```bash
rm -rf eva-cli
mkdir -p eva-cli
tar -C eva-cli -xzf eva-cli-<new-version>-<target>.tar.gz
./eva-cli/eva upgrade check --output json
```

Uninstall:

```bash
rm -rf eva-cli
```

## Package Metadata Dry-Run

The V1.11.2 release workflow verifies the GHCR package metadata without pulling
or republishing the image:

```bash
docker buildx imagetools inspect ghcr.io/yetmos/eva-cli@sha256:<digest>
```

That command is recorded in `release-evidence/package-ghcr.json` and then
copied into `release-evidence/release-distribution.evidence` as
`package.0.command`. `eva release check --distribution-evidence` requires the
dry-run status to be `passed`.

## Release Evidence

The release workflow generates `release-evidence/release-distribution.evidence`
using this key/value schema:

```text
format=eva.release.distribution_evidence.v1
version=<version>
source_tag=<tag>
source_commit=<full-sha>
docs.install=docs/en/release/install-upgrade-uninstall.md
docs.uninstall=docs/en/release/install-upgrade-uninstall.md
docs.upgrade=docs/en/release/install-upgrade-uninstall.md
smoke.0.os=windows
smoke.0.target=x86_64-pc-windows-msvc
smoke.0.artifact=eva-cli-<version>-x86_64-pc-windows-msvc.zip
smoke.0.package_format=zip
smoke.0.install_command=Expand-Archive ...
smoke.0.smoke_command=eva --version
smoke.0.uninstall_command=Remove the extracted Eva-CLI archive directory
smoke.0.upgrade_command=Replace the archive contents and run eva upgrade check --output json
smoke.0.status=passed
package.0.manager=ghcr
package.0.package=ghcr.io/yetmos/eva-cli
package.0.target=linux/amd64+linux/arm64
package.0.command=docker buildx imagetools inspect ghcr.io/yetmos/eva-cli@sha256:<digest>
package.0.status=passed
```

The release gate is:

```powershell
cargo run -- release check --distribution-evidence release-evidence/release-distribution.evidence --output json
```

It blocks when any of these are missing or not passed: Windows install smoke,
Linux install smoke, macOS install smoke, install/upgrade/uninstall docs, or
package-manager dry-run evidence.
