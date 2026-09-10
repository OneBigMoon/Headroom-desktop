# macOS 与 Windows 发布说明

本项目仅通过 `.github/workflows/release-community-macos.yml` 同时发布 macOS 与 Windows Community 版本。文件名为历史兼容保留；工作流只响应稳定的 `vX.Y.Z` 标签，不提供 `workflow_dispatch`，并要求版本文件、标签和发布说明一致。

## 发布流程

工作流把构建、汇总和发布权限分离：

1. Ubuntu 校验 Job 检查稳定标签、所有版本文件和 `.github/release-notes/<版本>.md`。
2. `macos-14` 与 `windows-latest` 构建 Job 在校验后并行运行；macOS Job 继续执行完整 release checks。
3. macOS Job 构建 universal `.app`/DMG，校验代码签名、双架构、DMG 和 updater tar.gz 签名。
4. Windows Job 构建 Windows 11 x64 预览版 NSIS `.exe` 及 Tauri `v1Compatible` updater `.nsis.zip`/`.sig`，并验证 updater 签名；首次联合发布前仍必须在干净 Windows 11 x64 上完成安装、首次启动、更新和卸载 smoke。Windows 10 尚未完成该门禁，不承诺当前版本已支持。
5. Ubuntu 汇总 Job 下载两个平台的只读 artifact，生成唯一的 `latest.json`，同时包含 Darwin 与 `windows-x86_64`。
6. 唯一有 `contents: write` 的发布 Job 创建 GitHub draft Release，并通过 GitHub 返回的远端 SHA-256 digest 逐个核对七个资产。
7. 只有远端 digest 全部匹配时才公开 Release；仓库必须预先启用 release immutability，该设置只保护启用后创建的 Release。
8. 公开后，工作流从 tag 下载地址重新下载全部资产，再从 `releases/latest/download/latest.json` 校验两个平台的 URL 与签名。

发布仓库是 [`OneBigMoon/Headroom-desktop`](https://github.com/OneBigMoon/Headroom-desktop)。

## 发布资产

每个稳定 Release 应包含以下资产：

```text
Headroom.Local.Community_<版本>_universal.dmg
Headroom.Local.Community_universal.app.tar.gz
Headroom.Local.Community_universal.app.tar.gz.sig
Headroom.Local.Community_<版本>_x64-setup.exe
Headroom.Local.Community_x64-setup.nsis.zip
Headroom.Local.Community_x64-setup.nsis.zip.sig
latest.json
```

原始 `.app` 仅用于构建阶段验签，不作为 Release 下载资产。DMG 和 NSIS `.exe` 面向手动安装；两个压缩 updater 资产由同一个 `latest.json` 按平台选择。updater endpoint 为：

```text
https://github.com/OneBigMoon/Headroom-desktop/releases/latest/download/latest.json
```

## 发布前不可变设置

首次发布前，在 GitHub 仓库 `Settings` 的 `Releases` 区域启用 `Enable release immutability`。该设置只作用于以后发布的 Release。工作流会在公开后立即检查 `immutable` 字段；若新公开的 Release 仍可修改，会立刻恢复为 draft 并让门禁失败，但发布前仍必须人工确认设置已启用，避免短暂公开窗口。

## 故障版本回滚

不可变 Release 发布后不能修改标签、资产、draft 或 prerelease 状态。常规回滚应从上一稳定提交恢复代码、提升到新的补丁版本并发布 recovery Release；不要移动标签、覆盖资产或复用故障版本号。

若 recovery Release 尚未准备好且必须立即撤下故障版本，只能在明确确认后删除该 Release，使 `releases/latest` 回到上一稳定版。保留故障标签作为审计记录，且永远不要复用该标签名：

```bash
set -euo pipefail

REPO="OneBigMoon/Headroom-desktop"
BAD_TAG="vX.Y.Z"
GOOD_TAG="vA.B.C"
GOOD_VERSION="${GOOD_TAG#v}"

bad_id="$(gh api "repos/${REPO}/releases/tags/${BAD_TAG}" --jq '.id')"
gh api --method DELETE "repos/${REPO}/releases/${bad_id}"

test "$(gh api "repos/${REPO}/releases/latest" --jq '.tag_name')" = "${GOOD_TAG}"

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT
feed="https://github.com/${REPO}/releases/latest/download/latest.json"
updater_name="Headroom.Local.Community_universal.app.tar.gz"
windows_updater_name="Headroom.Local.Community_x64-setup.nsis.zip"

curl -fsSL -H 'Cache-Control: no-cache' "${feed}" -o "${tmp}/latest.json"
jq -e --arg version "${GOOD_VERSION}" --arg tag "${GOOD_TAG}" '
  .version == $version
  and ([.platforms[]?.url] | length > 0)
  and ([.platforms[]?.url] | all(contains("/download/" + $tag + "/")))
' "${tmp}/latest.json" >/dev/null

for name in \
  "Headroom.Local.Community_${GOOD_VERSION}_universal.dmg" \
  "${updater_name}" \
  "${updater_name}.sig" \
  "Headroom.Local.Community_${GOOD_VERSION}_x64-setup.exe" \
  "${windows_updater_name}" \
  "${windows_updater_name}.sig" \
  "latest.json"
do
  url="https://github.com/${REPO}/releases/download/${GOOD_TAG}/${name}"
  curl -fsSL "${url}" -o "${tmp}/${name}"
  local_digest="sha256:$(shasum -a 256 "${tmp}/${name}" | awk '{print $1}')"
  remote_digest="$(gh api "repos/${REPO}/releases/tags/${GOOD_TAG}" \
    --jq ".assets[] | select(.name == \"${name}\") | .digest")"
  test "${local_digest}" = "${remote_digest}"
done

printf '%s' "${HEADROOM_UPDATER_PUBLIC_KEY}" > "${tmp}/public-key.b64"
cargo run --quiet --manifest-path src-tauri/Cargo.toml \
  --example verify_updater_signature -- \
  "${tmp}/${updater_name}" "${tmp}/${updater_name}.sig" "${tmp}/public-key.b64"
cargo run --quiet --manifest-path src-tauri/Cargo.toml \
  --example verify_updater_signature -- \
  "${tmp}/${windows_updater_name}" "${tmp}/${windows_updater_name}.sig" "${tmp}/public-key.b64"
```

已安装故障版本的客户端不会自动降级，仍需手动重装上一稳定版 DMG 或 NSIS 安装包。删除故障 Release 后应立即从上一稳定提交发布一个更高补丁版本；旧客户端随后只会看到恢复后的稳定 feed。

## 下载

从 [GitHub Releases](https://github.com/OneBigMoon/Headroom-desktop/releases/latest) 下载稳定版 macOS universal DMG，或同一 Release 中的 Windows 11 x64 预览版 NSIS `.exe`。

## 签名与密钥

Community 使用 ad-hoc macOS 代码签名，以及供 macOS/Windows updater 共用的独立 Tauri 签名密钥。工作流需要仓库变量 `HEADROOM_UPDATER_PUBLIC_KEY` 和 secrets `TAURI_SIGNING_PRIVATE_KEY`、`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。这不等同于 Apple Developer ID 签名、Apple 公证或 Windows Authenticode 签名；当前 NSIS 安装包可能触发 Windows SmartScreen 的“未知发布者”提示，updater 签名不能替代 Authenticode 的发布者信任。

## 本机构建

```bash
git clone https://github.com/OneBigMoon/Headroom-desktop.git
cd Headroom-desktop
npm ci
npm run build:mac:local
```

macOS 本地构建要求 macOS 14 或更高版本、Node.js/npm 和 Rust；产物位于 `src-tauri/target/release/bundle`。Windows 11 x64 预览版构建还需要 Microsoft C++ Build Tools、WebView2 和 Tauri Windows prerequisites；Windows 10 未完成 smoke，不构成支持承诺。发布用 `.nsis.zip`/`.sig` 只由带 updater 私钥的 CI 生成。

## Homebrew

当前没有维护中的 Homebrew tap 或已核实的 cask 资产，因此项目不提供 Homebrew 安装承诺。请使用 GitHub Release DMG 或 NSIS 安装包；待固定资产 URL 和 SHA 可复核后再单独恢复 Homebrew 文档。
