# CClear 开发规范（所有执行者必读）

CClear 是一个个人自用的 C 盘清理桌面工具。唯一有效的设计文档是 `docs/plan-final.md`，当前任务的说明在 `docs/tasks/Txx.md`。**只做当前任务书要求的内容**，不要提前实现后续任务，也不要重构与当前任务无关的代码。任务书与本文件冲突时，以本文件的“硬性规则”为准，并在最终说明中指出冲突。

## 1. 技术栈与目录结构（固定，不得更改）
- Rust edition 2024，Cargo workspace，根目录 `Cargo.toml` 的 members 为 `crates/cclear-core`、`src-tauri`。
- Tauri 2（`src-tauri`，crate 名 `cclear-app`）+ React + TypeScript + Vite（前端在仓库根目录：`package.json`、`index.html`、`src/`）。
- 前端不引入 UI 组件库、路由库或状态管理库。样式用纯 CSS。只有 T08 可以引入 `d3-hierarchy`。
- Rust 允许的依赖：`windows`（只启用需要的 feature）、`serde`、`serde_json`、`thiserror`、`rayon`、`blake3`、`uuid`、`time`。仅 dev 依赖：`tempfile`。需要其他依赖时，必须在最终说明里写明理由。

```
crates/cclear-core/src/
  lib.rs  error.rs  model.rs       公共类型（Tier、Recovery、Category、SizeInfo 等）
  win/          对 Win32 的薄封装（目录枚举、Known Folder、卷、进程、注册表、Restart Manager）
  scan/         扫描器
  rules/        内置规则目录（catalog.rs）、禁区（forbidden.rs）、候选解析（resolve.rs）、adapters/
  plan/         冻结清单
  guard/        路径护栏：全项目唯一允许删除、改名、创建或删除 junction 的地方
  exec/         执行器（executor.rs）、外部命令（command.rs）
  journal/      追加式操作日志
  quarantine/   隔离区事务
  migrate/      迁移
  elevation/    提权 helper
  special/      回收站等特殊操作
  envstate.rs   环境状态卡
  ai/           AI 咨询：地址校验、脱敏、提问（T20a）
crates/cclear-core/tests/   所有测试都放在这里（集成测试）
src-tauri/src/  main.rs  lib.rs  state.rs  commands/*.rs   只做 IPC 薄层，不写业务逻辑
src/            main.tsx  App.tsx  api.ts（所有 invoke 的类型化封装）  types.ts  pages/  components/  styles.css
```

## 2. 硬性规则（违反即审查不通过）
1. **破坏性文件操作只能出现在 `crates/cclear-core/src/guard/`**，包括删除、改名、移动、设置 delete disposition、创建或删除 reparse point。清空回收站只能在 `special/recycle_bin.rs`。启动外部进程只能在 `exec/command.rs` 和 `elevation/`。`scripts/check-forbidden-apis.ps1` 会自动检查。不得改动这个脚本，也不得用别名、宏或字符串拼接绕过检查。
2. `unsafe` 只允许出现在 `win/`、`guard/`、`special/`、`elevation/` 中。每个 `unsafe` 块要有一行 `// SAFETY:` 说明。
3. 测试和验收命令**绝不能触碰真实用户数据或系统路径**。测试只能在 `tempfile::tempdir()` 创建的目录里操作，规则解析类测试通过注入的“根目录映射”指向临时目录。
4. 不联网、不做遥测、不自动更新。不以管理员身份运行主程序（manifest 保持 asInvoker）。唯一例外是 AI 咨询（T20a 起，见 `docs/plan-final.md` §10）：只在用户点击“问 AI”时，向用户自己填写的地址发一次请求；地址只允许 https 或本机 http，禁止重定向；发送前必须经过 `ai::redact` 脱敏；网络请求代码只能在 `win/http.rs`，调用方只能是 `ai/`；API Key 只能以 DPAPI 加密形式落盘，不返回前端、不进日志；AI 的回答只用于展示，不得触发或改变任何清理、隔离、迁移操作。
5. 不强杀进程、不停第三方服务、不接管所有权、不修改 ACL（T12 创建隔离根目录时设置自身 DACL 除外）、不清除只读属性。遇到这些情况时跳过该项，并在结果中说明原因。
6. 任何失败都只能导致“跳过并报告”。不允许扩大删除范围、退回直接删除目录或提升权限去兜底。
7. 前端只向后端传 ID（计划 ID、规则 ID、条目 ID）和用户选择。**任何路径都不能从前端直接传入执行类命令**。T03 扫描路径和 T17 迁移源这类分析输入，必须先经过 `guard::validate_external_path`。
8. Tauri：不使用 `tauri-plugin-fs/shell/opener/process/http`；capability 只授予本应用的命令权限，不写 `remote`；不能使用 `core:default`。新增命令必须同时修改四处：`src-tauri/src/commands/*.rs`、`src-tauri/build.rs` 中 `AppManifest::commands` 的列表、`src-tauri/capabilities/main.json`、`src/api.ts`。
9. 不暂存、不提交、不推送。提交由 Bridge 在审查通过后完成。

## 3. 构建环境
- 所有 cargo 命令前先设置 `$env:CARGO_TARGET_DIR='G:\cclear-target'`（C 盘空间紧张，worktree 在 C 盘）。
- 不运行 `cargo install`、`tauri build`（T18 除外），也不运行全局 npm 安装。
- 前端构建：`npm ci --no-audit --no-fund && npm run build`。生成的 `dist/` 是 `cargo clippy --workspace` 的前置条件（Tauri 编译时会检查 frontendDist 目录是否存在）。

## 4. 代码约定
- 公共错误类型用 `thiserror`，放在 `error.rs`。Tauri 命令返回 `Result<T, String>`，错误信息用中文、让用户能看懂。
- 路径在内部统一用 `PathBuf`。调用 Win32 时需要的话转换为 `\\?\` 形式（`win::to_verbatim`）。大小写比较用 `CompareStringOrdinal(..., TRUE)`，不用 `to_lowercase`。
- 用户目录一律通过 `SHGetKnownFolderPath` 解析（`win::known_folder`），不读取 `%USERPROFILE%` 等环境变量。
- 跨 IPC 的结构体用 `#[serde(rename_all = "camelCase")]`，并在 `src/types.ts` 中手写对应的 TS 类型。
- UI 文本全部用中文。大小统一显示为 `x.xx GB / MB`（二进制单位）。
- 注释只写不明显的原因或约束，不复述代码在做什么。

## 5. 测试策略（避免冗余）
- 只为**会造成数据损失或误判的逻辑**写测试：路径护栏、执行器、冻结清单校验、隔离事务、迁移、残留适配器判定、硬链接去重。
- 不为 UI、DTO 序列化、getter、格式化函数、简单的 Win32 封装写测试。不做快照测试，不写 mock 框架。
- 每个任务书会列出必须编写的测试用例。不要额外添加与之重复的测试。
- 测试需要 junction 时，在 `tests/` 中用 `cmd /c mklink /J` 创建（测试代码不受规则 1 限制，但只能操作临时目录）。
