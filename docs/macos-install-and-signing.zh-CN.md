# macOS 安装、签名与自动更新说明

Headroom Local Community（GitHub 项目：[`OneBigMoon/Headroom-desktop`](https://github.com/OneBigMoon/Headroom-desktop)）是非官方免费社区版本，不包含官方付费服务、官方签名证书或官方更新通道。

## 推荐安装方式

1. 从 [GitHub Releases](https://github.com/OneBigMoon/Headroom-desktop/releases/latest) 下载最新稳定版 DMG。
2. 打开 DMG，把 `Headroom Local Community.app` 拖入“应用程序”。
3. 首次启动优先在 Finder 中右键应用并选择“打开”。

当前社区发行版使用 ad-hoc 本地代码签名，并使用独立的 Tauri updater 签名验证更新包。这不等同于 Apple Developer ID 签名或 Apple 公证。

## Gatekeeper 提示

先把应用复制到 `/Applications`，然后只对这个应用清除下载隔离属性：

```bash
xattr -dr com.apple.quarantine "/Applications/Headroom Local Community.app"
codesign --verify --deep --strict --verbose=2 "/Applications/Headroom Local Community.app"
```

不要为了运行单个社区应用而全局关闭 Gatekeeper，也不要执行 `sudo spctl --master-disable`。

## 本机从源码构建

```bash
git clone https://github.com/OneBigMoon/Headroom-desktop.git
cd Headroom-desktop
npm ci
npm run build:mac:local
```

构建产物：

```text
src-tauri/target/release/bundle/macos/Headroom Local Community.app
src-tauri/target/release/bundle/dmg/Headroom Local Community_<版本>_<架构>.dmg
```

## 自动更新

Tauri updater 使用 Community 独立发布密钥，更新地址为：

```text
https://github.com/OneBigMoon/Headroom-desktop/releases/latest/download/latest.json
```

应用不会静默安装更新；下载和安装前会显示确认。私钥只能保存在发布者本机或 GitHub Secrets，不能提交到仓库。

## 回滚与 Homebrew

若新版本异常，已安装该版本的用户需要从上一稳定版 GitHub Release 重新安装 DMG；Tauri updater 不会自动降级。应用数据保存在独立的 `HeadroomLocalCommunity` 目录，不会覆盖官方应用的数据。

发布者不能修改不可变 Release 的标签或资产。常规回滚应从上一稳定提交发布更高补丁版本；若必须立即撤下故障版本，只能在明确确认后删除该 Release，并且永远不能复用其标签名。`releases/latest/download/latest.json` 恢复后，还要核对版本、下载 URL、资产摘要和 updater 签名；完整命令见 [`docs/macos-release.md`](./macos-release.md)。

项目当前没有维护中的 Homebrew tap 或 cask 安装入口。
