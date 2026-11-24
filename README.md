# CClear

CClear 是一款面向 Windows 10/11 的极简、高收益 C 盘空间清理桌面工具（Tauri 2 + Rust + React/TypeScript），聚焦空间回收本身：扫描快、清理收益高、安全护栏清晰，默认纯离线运行。

## 直接下载安装

仓库中已附带编译好的 Windows x64 简体中文安装程序（当前用户安装，无需安装时管理员权限）：

- [releases/CClear_0.1.0_x64-setup.exe](releases/CClear_0.1.0_x64-setup.exe)

> 说明：当前安装包未进行 Authenticode 代码签名，在其他电脑首次运行时可能触发 Windows SmartScreen 提示，点击「更多信息 → 仍要运行」即可。

## 核心功能

界面精简为三个页面：

- **一键清理**
  - C 盘占用概览与一键并发扫描（内置声明式规则库 + 通用 Electron/Chromium 缓存、UWP 缓存、卸载残留启发式探测）。
  - 五大分类手风琴（系统垃圾、应用缓存、开发缓存、系统更新缓存、软件卸载残留）+ 独立回收站与系统深度瘦身卡。
  - **系统深度瘦身**：休眠文件切换（关闭 / 精简 / 完整）、DISM WinSxS 组件清理（默认不带 `/ResetBase`）、清理最旧系统卷影副本、WSL2 / Docker VHDX 原生虚拟磁盘压缩、页面文件状态只读查看。
  - 清理前自动检测占用进程，提供温和关闭窗口（`WM_CLOSE`）、重新检测或单文件跳过；支持将锁定的系统文件排入重启后删除（`MoveFileExW`）。
- **空间分析**
  - 支持免提权标准并发扫描与按需提权的 NTFS `$MFT` 极速扫描（失败自动降级为标准模式）。
  - 提供目录列表、Treemap 图形视图、Top 200 大文件三种视图，支持在资源管理器定位与安全移入回收站。
- **历史与设置**
  - 追加式清理历史日志（保留最近 30 天，可展开逐条详情与跳过原因）。
  - 路径白名单管理、外置 JSON 规则包导入（自动做路径与安全校验并展示规则目标摘要）、默认扫描模式与重启删除偏好设置。

## 安全设计要点

- **路径护栏收口**：所有删除、移入回收站与重启删除操作均集中在 `crates/cclear-core/src/guard/`，逐组件校验路径、拒绝符号链接 / Junction 穿越与系统/用户个人目录禁区。
- **安全移入回收站**：通过 Windows `IFileOperation` + `IFileOperationProgressSink` 执行回收，若回收站被禁用或文件超限导致无法进入回收站，直接中止并报告，绝不静默降级为永久删除。
- **权限最小化**：主程序保持 `asInvoker`（正常启动免 UAC），管理员清理、深度瘦身与 `$MFT` 扫描统一汇总为单次 UAC 批处理提权执行。
- **纯离线与温和策略**：不联网、不做遥测、不自动更新；不强杀进程（禁止 `TerminateProcess`）、不修改 ACL、不接管所有权，单文件被锁时跳过并如实报告。

架构与接口文档见：
- [docs/v2-architecture.md](docs/v2-architecture.md)
- [docs/v2-ipc-contract.md](docs/v2-ipc-contract.md)
- [docs/v2-review-checklist.md](docs/v2-review-checklist.md)

## 本地构建与验证

环境要求：Windows 10/11、Rust（edition 2024 + MSVC 工具链）、Node.js 20+、WebView2 运行时。

```powershell
# 建议将 Cargo 编译输出目录放到非 C 盘
$env:CARGO_TARGET_DIR='G:\cclear-target'
npm ci --no-audit --no-fund

# 开发模式运行
npm run tauri dev

# 打包 Windows NSIS 安装程序（输出至 $env:CARGO_TARGET_DIR\release\bundle\nsis\）
npm run tauri -- build --bundles nsis

# 质量与安全检查
npm run build
pwsh -NoProfile -File scripts/check-forbidden-apis.ps1
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
