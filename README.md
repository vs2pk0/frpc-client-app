# frpc-client-app

FRPC 桌面客户端，支持 macOS 和 Windows，提供简化配置、运行时管理、更新导入和一键启动/停止。

## 功能

- 快捷编辑 `frpc.toml`，支持多个代理配置。
- 查看当前运行状态、日志、工作区和 frpc 二进制路径。
- 从 FRP GitHub Release 检测并导入 macOS / Windows 运行时。
- 打包为 macOS DMG、Windows MSI 和 NSIS 安装包。

## 本地开发

```bash
npm ci
npm run tauri:dev
```

## 本地打包

```bash
npm run tauri -- build
```

构建产物位于 `src-tauri/target/release/bundle/`。

## GitHub Releases

仓库包含 `.github/workflows/release.yml`。推送 `v*` 标签后会自动在 GitHub Actions 中分别构建 macOS 和 Windows 安装包，并发布到 Releases。

```bash
git tag v0.1.1
git push origin v0.1.1
```
