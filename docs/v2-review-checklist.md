# CClear v2 最终审查清单（Opus 5.5 审查基线）

本文档用于指导最终高强度审查，覆盖安全性、正确性、前后端契约一致性和产品体验。

## 1. 安全与防误删红线（P0 阻断项）
- [ ] `scripts/check-forbidden-apis.ps1` 未被篡改且执行输出 `OK`。
- [ ] 所有破坏性文件操作（直删、改名、送回收站 `SHFileOperationW`、重启删除 `MoveFileExW`）严格局限在 `crates/cclear-core/src/guard/`，且调用前均验证禁区与 reparse point/junction 逃逸。
- [ ] 外部导入的 JSON 规则包严格禁止 `command`、`systemOp`、绝对路径/UNC/`..`/ADS/空相对路径，且任何命中系统禁区的路径都会被硬性拦截。
- [ ] `clean_close_processes` 只能优雅发送 `WM_CLOSE` 给当前扫描冲突列表里的用户进程，严禁调用 `TerminateProcess`，严禁对系统核心进程（`explorer.exe`、`csrss.exe`、`lsass.exe`、`svchost.exe` 等）发关闭消息。
- [ ] 卸载残留探测器（`heuristics/orphans.rs`）必须拥有充分的系统/运行时/主流厂商保护白名单、30 天修改时间门槛、默认不勾选、且强制走 `recycleBin` 模式（绝不直删）。
- [ ] Electron/Chromium 启发式探测器只清理可再生缓存叶子目录（`Cache`/`Code Cache`/`GPUCache`/`GrShaderCache`/`ShaderCache`/`DawnCache`/`Crashpad\reports`），绝不触碰 `Local Storage`、`IndexedDB`、`Cookies`、`Login Data`、`Bookmarks`。
- [ ] UWP 探测器只触碰 `Packages\*\TempState` 与 `Packages\*\AC\INetCache`，绝不触碰 `LocalState`/`RoamingState`/`Settings`。
- [ ] 前端不传递任意文件系统路径给任何删除/清理/回收站命令（仅传 ID）；白名单路径和空间扫描根路径经过 `guard::validate_external_path`。
- [ ] Tauri capabilities 零 `core:default`、零 `remote`、零危险插件；25 个命令在 `commands/`、`lib.rs`、`build.rs`、`capabilities/main.json`、`src/api.ts` 五处完全对齐。
- [ ] 所有 `unsafe` 块仅在 `win/`、`guard/`、`special/`、`elevation/` 中，每处有准确 `// SAFETY:` 注释，无悬垂指针、句柄泄漏或越界切片。
- [ ] 提权 helper（`--elevated`）严格校验参数，不接受任意用户路径；若存在重启删除文件列表，必须在 helper 内重新逐条校验禁区、reparse point 与允许锚点，防止低权限进程篡改清单借提权删任意文件（特权提升漏洞检查！）。

## 2. 架构与正确性（P1 重要项）
- [ ] NTFS `$MFT` 扫描器正确处理非驻留 `$DATA` runs、fixup array（USA 更新序列数组校验与还原）、符号扩展偏移、目录树父子归属、大文件收集，并在非 NTFS / 权限不足 / 异常时无崩溃降级到标准扫描。
- [ ] 统一清理管线在进程未关闭时能够按**单文件跳过**锁文件，照常删除同目录其余未锁文件，不会规则级一票否决。
- [ ] 多个管理员清理项在一次「立即清理」中合并为**单次** UAC 调用，UAC 取消时正常降级报告，不影响已完成的普通权限清理结果。
- [ ] 深度瘦身操作（`hiberfil`、`DISM`、`VSS`、`WSL compact`）命令参数使用 System32 绝对路径、干净环境变量，DISM 明确不带 `/ResetBase`。
- [ ] 全部集成测试（`cargo test --workspace`）均在 `tempfile::tempdir()` 中运行，覆盖：路径护栏竞态、外置规则包恶意输入拒绝、Electron/UWP/Orphan 启发式防误判、文件级锁跳过、MFT 解析单元测试、提权参数校验。

## 3. 前端体验与构建验证（P1/P2）
- [ ] 前端极致精简为 3 个导航页：`一键清理`、`空间分析`、`历史与设置`，无残留的旧 v1 页面、死代码或坏引用。
- [ ] 首页直观展示 C 盘红/橙/绿水位条、一键扫描、按场景分类的树形勾选、冲突进程提示弹窗、实时释放进度与系统深度瘦身大卡片。
- [ ] `npm run build`（`tsc && vite build`）零错误；`cargo clippy --workspace --all-targets -- -D warnings` 零警告；`cargo test --workspace` 全绿。
