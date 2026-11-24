# CClear 开发规范（v2 · 所有执行者必读）

CClear 是一款面向 Windows 10/11 用户的极简、高收益 C 盘空间清理桌面工具。唯一有效的设计文档是 `docs/v2-architecture.md`，前后端接口契约见 `docs/v2-ipc-contract.md`，审查基线见 `docs/v2-review-checklist.md`。

## 1. 技术栈与目录结构（固定）
- Rust edition 2024，Cargo workspace，根目录 `Cargo.toml` 的 members 为 `crates/cclear-core`、`src-tauri`。
- Tauri 2（`src-tauri`，crate 名 `cclear-app`）+ React + TypeScript + Vite（前端在仓库根目录：`package.json`、`index.html`、`src/`）。
- 前端不引入 UI 组件库、路由库或状态管理库；样式用纯 CSS；保留 `d3-hierarchy` 用于空间分析 Treemap。
- Rust 复用现有依赖：`windows`（只启用需要的 feature）、`serde`、`serde_json`、`thiserror`、`rayon`、`blake3`、`uuid`、`time`；dev 依赖：`tempfile`。不随意引入新 crate。

```
crates/cclear-core/
  rules/builtin.json            内置声明式规则库
  src/
    lib.rs  error.rs  model.rs  settings.rs
    win/          Win32 薄封装（目录枚举、Known Folder、卷、进程与 WM_CLOSE、注册表、Restart Manager、NTFS MFT）
    guard/        路径护栏：全项目唯一允许删除、改名、送回收站、重启删除与操作 reparse point 的地方
    rules/        声明式规则引擎（schema.rs、engine.rs）、禁区（forbidden.rs）、命令（commands.rs）、heuristics/
    orchestrator/ 统一扫描与清理编排器、系统深度瘦身状态
    exec/         文件级尽力清理器（file_cleaner.rs）、外部命令（command.rs）
    elevation/    单次 UAC 批处理提权 helper（系统文件清理、深度瘦身、重启删除、MFT 扫描）
    scan/         并行目录扫描器与空间树模型
    special/      回收站查询与清空
    journal/      追加式清理历史日志
crates/cclear-core/tests/       全部集成测试
src-tauri/src/                  薄 IPC 层，不写业务逻辑
src/                            极简 3 页前端（一键清理 / 空间分析 / 历史与设置）
```

## 2. 硬性安全规则（违反即审查不通过）
1. **破坏性文件操作只能出现在 `crates/cclear-core/src/guard/`**，包括删除、改名、移动、送回收站（`SHFileOperationW`/`IFileOperation`）、设置重启删除（`MoveFileExW`）、设置 delete disposition、操作 reparse point。清空回收站只能在 `special/recycle_bin.rs`。启动外部进程只能在 `exec/command.rs` 和 `elevation/`。**严禁改动 `scripts/check-forbidden-apis.ps1`**，也不得绕过检查。
2. `unsafe` 只允许出现在 `win/`、`guard/`、`special/`、`elevation/` 中。每个 `unsafe` 块必须有一行 `// SAFETY:` 说明。
3. 测试和验收命令**绝不能触碰真实用户数据或系统路径**。所有测试只能在 `tempfile::tempdir()` 中操作，通过注入的 `RootMap` 指向临时目录。
4. 默认纯离线运行：不联网、不做遥测、不自动更新。主程序 manifest 保持 `asInvoker`（正常启动免 UAC），高权限操作统一走 `--elevated` 批处理 helper。
5. 不强杀进程（禁止 `TerminateProcess`；仅允许对用户确认的冲突应用窗口发送优雅 `WM_CLOSE`）、不停第三方服务、不接管所有权、不修改 ACL、不清除只读属性。单文件被锁时跳过该文件并继续清理其余未锁文件，或由用户选择排入系统重启删除。
6. 任何失败都只能跳过并如实报告，不允许扩大删除范围或越权兜底。
7. 前端只向后端传 ID（`scanId`、`itemId`、`nodeId`、`fileIndex`、`runId`、`packId`）。**任何执行/删除类命令都不接受前端自由路径**；白名单与空间扫描起点等路径必须先经过 `guard::validate_external_path`。
8. Tauri：不使用 `tauri-plugin-fs/shell/opener/process/http`；capability 只授予本应用的命令权限，不写 `remote`，不用 `core:default`。所有命令必须同步维护五处：`src-tauri/src/commands/`、`src-tauri/src/lib.rs`、`src-tauri/build.rs`、`src-tauri/capabilities/main.json`、`src/api.ts`。
9. 不执行 `git commit`、`git push` 或破坏性 Git 操作，除非用户明确要求。

## 3. 构建环境
- 所有 cargo 命令前先设置 `$env:CARGO_TARGET_DIR='G:\cclear-target'`。
- 不运行 `cargo install`、`tauri build`，也不运行全局 npm 安装。
- 前端构建：`npm run build`（生成的 `dist/` 是 `cargo clippy --workspace` 的前置条件）。
- 静态安全脚本检查需使用 PowerShell 7：`pwsh -File scripts/check-forbidden-apis.ps1`（该脚本为无 BOM UTF-8，Windows PowerShell 5.1 无法解析）。

## 4. 代码约定
- 公共错误类型用 `thiserror`；Tauri 命令返回 `Result<T, String>`，错误信息用中文、清晰易懂。
- 内部路径统一用 `PathBuf`，调用 Win32 长路径用 `win::to_verbatim`，大小写比较用 `win::equals_ignore_case`（`CompareStringOrdinal`）。
- 用户目录一律通过 `SHGetKnownFolderPath`（`win::known_folder`）解析。
- 跨 IPC 结构体使用 `#[serde(rename_all = "camelCase")]`，与 `src/types.ts` 严格一致。
- UI 文本全部用中文，体积统一二进制单位（`x.xx GB / MB / KB`）。
