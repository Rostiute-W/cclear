# CClear v2 架构设计文档

> 本文档取代 `docs/plan-final.md`（v1），作为 CClear v2 的唯一有效架构设计基线。  
> 研发分工：后端（Rust + Tauri IPC）由 **Codex** 实现；前端（React + TypeScript + CSS）由 **Antigravity** 实现；最终代码与安全审查由 **Factory Droid · Opus 5.5（最高思考强度 `max`）** 完成。

---

## 1. 产品定位与核心原则

1. **面向广大 Windows 10/11 用户的极简、高收益 C 盘空间清理工具**（专治 C 盘爆红）。
2. **纯空间释放边界**：聚焦系统垃圾、浏览器缓存、常用软件与通用 Electron/UWP 缓存、开发者黑洞、卸载孤儿残留、系统深度瘦身（休眠/WinSxS/VSS/WSL 虚拟盘压缩）与极速空间分析。**不做**注册表清理，**不做**隐私痕迹/空闲扇区反取证覆写。
3. **移除 v1 本机私有规则与冗余复杂度**：
   - 删除所有个人私有规则（`codex-runtimes`、`droid.exe.old`、`LeAppStore`、`MathWorksServiceHost`、`qianfan`、`DeskBox`、`NGR` 等）。
   - 删除自定义隔离区（`quarantine/`）、迁移功能（`migrate/`）、AI 咨询（`ai/`）以及繁琐的“冻结清单二次预览”流程。
   - **安全不降级**：保留并强化底层路径护栏（`guard/`）、系统硬性禁区表与 `scripts/check-forbidden-apis.ps1` 静态红线；高风险项（卸载残留、空间分析页手动删除）默认送入 **Windows 原生回收站**，兼顾极简 UI 与可撤销安全网。
4. **正常启动、按需提权**：主程序 manifest 保持 `asInvoker`，日常启动不弹 UAC。涉及系统目录清理、系统深度瘦身、重启后删除或 NTFS `$MFT` 极速扫描时，通过内置 `--elevated` 批处理 helper **单次 UAC 授权批量执行**。

---

## 2. 技术栈与目录结构

- **Rust（edition 2024）**：Cargo workspace（`crates/cclear-core`、`src-tauri`），零新增未批准 crate（复用现有 `windows`、`serde`、`serde_json`、`thiserror`、`rayon`、`uuid`、`time`、`tempfile`）。
- **Tauri 2 + React 19 + TypeScript + Vite**：纯 CSS，保留 `d3-hierarchy` 用于空间分析 Treemap，不引入其他 UI/路由/状态库。

```text
crates/cclear-core/
  rules/
    builtin.json            内置声明式规则库（include_str! 打包，引擎与规则彻底解耦）
  src/
    lib.rs  error.rs  model.rs
    win/
      mod.rs                KnownFolder、is_elevated、to_verbatim、大小写比较
      dirinfo.rs            底层 Win32 目录枚举
      volume.rs             卷信息与空闲空间
      process.rs            进程枚举 + 优雅关闭窗口（WM_CLOSE，禁止 TerminateProcess）
      registry.rs           注册表只读查询（休眠、分页、WSL、Uninstall 已装程序表）
      restart_manager.rs    文件占用检测
      mft.rs                [NEW] NTFS $MFT 极速只读扫描（管理员下 1~3 秒建全盘树）
    guard/
      mod.rs  external.rs  forbidden.rs  path.rs
      recycle.rs            [NEW] SHFileOperationW 送入 Windows 原生回收站（先过禁区与 reparse 检查）
      reboot.rs             [NEW] MoveFileExW(MOVEFILE_DELAY_UNTIL_REBOOT) 重启后删除
    rules/
      mod.rs
      schema.rs             [NEW] 声明式规则 JSON Schema、校验器与外置规则包加载（仿火绒规则包机制）
      engine.rs             [NEW] 规则解析、通配符展开、Chromium Profile 枚举、体积统计
      heuristics/
        mod.rs
        electron.rs         [NEW] 通用 Chromium/Electron 全家桶缓存探测器
        uwp.rs              [NEW] UWP / Microsoft Store 临时缓存探测器
        orphans.rs          [NEW] 对照 Uninstall 注册表的卸载孤儿目录探测器
      commands.rs           开发工具官方清理命令（npm/pnpm/pip/uv/conda/dotnet）
      forbidden.rs          硬性系统禁区目录表（不可被任何规则或规则包解除）
    orchestrator/
      mod.rs                [NEW] 统一扫描/清理编排器（JunkScanResult、CleanItem、统一流式执行）
      deep_slim.rs          [NEW] 系统深度瘦身状态探测（休眠/WinSxS/VSS/WSL vhdx/分页文件）
    exec/
      mod.rs  command.rs    外部命令安全执行器
      file_cleaner.rs       [NEW] 文件级尽力清理器（跳过被锁单文件、记录冲突、可选排入重启删除）
    elevation/
      mod.rs  args.rs  helper.rs  launch.rs
      system_ops.rs         [NEW] 提权系统清理与深度瘦身（Windows\Temp、SoftwareDistribution、
                            Logs、WER、Windows.old、DISM、DO、hiberfil、VSS、WSL compact、
                            MFT scan）
    scan/
      mod.rs                并行目录扫描器 + MFT 扫描结果统一适配为 ScanResult
    special/
      mod.rs  recycle_bin.rs C 盘回收站查询与清空
    journal/
      mod.rs                追加式清理历史日志（v2 统一记录 CleanRun）
    settings.rs             [NEW] 用户设置（白名单路径、重启删除默认项、Temp 最小保留小时数、外置规则包管理）

crates/cclear-core/tests/   全部集成测试（仅操作 tempfile::tempdir()）
src-tauri/src/              薄 IPC 层（commands/ 仅做状态锁与参数转换）
src/                        极简 3 页前端（一键清理 / 空间分析 / 历史与设置）
```

---

## 3. 五大清理场景分类与默认勾选策略

前端不再按“安全/谨慎/风险”拆表，而是按用户直觉的 **5 大垃圾分类 + 深度瘦身卡片区** 展示。底层 `defaultSelected` 与 `deleteMode` 控制安全边界：

| 分类 (`CleanCategory`) | 包含的核心项目 | 默认勾选 | 删除模式 (`DeleteMode`) | 权限 |
|---|---|---|---|---|
| **`system` 系统垃圾** | `%LOCALAPPDATA%\Temp`（>24h）、`Windows\Temp`（>24h）、`Windows\SoftwareDistribution\Download`、传递优化缓存、`Windows\Logs`（.log/.etl/.cab >7d）、用户+系统 WER 与崩溃转储（CrashDumps / Minidump / MEMORY.DMP）、Defender 扫描历史、缩略图/图标缓存、DirectX/NVIDIA/AMD/Intel 着色器缓存、C 盘回收站 | **是**（`$SysReset` / `$WINDOWS.~BT` / `Windows.old` 默认**否**并附回退提示） | `direct` / `recycleBinEmpty` / `systemOp` | 用户项免提权；系统项单次 UAC 批处理 |
| **`browser` 浏览器缓存** | Edge、Chrome、Firefox、Brave、Opera、Vivaldi、Arc、夸克、360 极速、QQ 浏览器（仅清 `Cache`、`Code Cache`、`GPUCache`、`GrShaderCache`、`ShaderCache`、`cache2`，**绝不碰** Cookie/密码/书签/历史） | **是** | `direct` | 免提权 |
| **`app` 常用软件缓存** | ① 内置规则：微信（log/cache/temp）、QQ/NTQQ（Cache/log）、钉钉（log/Temp/xcdn/resource_cache）、飞书/Lark、WPS（log/download/update）、显卡驱动解包残留（`C:\NVIDIA`、`C:\AMD`、`NVIDIA\Downloader`、`ota-artifacts`）、Steam/Adobe/影音常见缓存；<br>② **通用 Electron/Chromium 启发式探测器**（自动覆盖 VS Code、Cursor、Discord、Notion、Obsidian、Slack 等）；<br>③ **UWP 应用临时缓存探测器**（`Packages\*\TempState` & `AC\INetCache`） | **是**（驱动解包残留、微信/钉钉非日志缓存可按安全度标提示） | `direct`（涉及 ProgramData/系统根的标记 `requiresAdmin`） | 绝大部分免提权 |
| **`dev` 开发者缓存** | npm、pnpm prune、yarn、bun、pip、uv prune、cargo（registry/cache & git/db）、NuGet http/temp、Go build cache（默认勾选）；<br>uv clean、conda clean、NuGet global-packages、Gradle caches、Playwright/Puppeteer 浏览器（默认**不勾**，标“下次需重新下载”） | 安全可重建项**是**；重下载成本高项**否** | `direct` / `command` | 免提权 |
| **`leftover` 卸载残留** | 对照 `Uninstall` 注册表 + 运行进程 + 强内置厂商白名单，识别 `%LOCALAPPDATA%`、`%APPDATA%` 中 >30 天、≥10 MB 的孤儿目录 | **否**（需用户确认） | **`recycleBin`**（移入 Windows 原生回收站，无法回收时拒绝直删） | 免提权 |
| **`deepSlim` 系统深度瘦身** | ① 休眠文件 `hiberfil.sys`（关闭 / 精简约一半体积仅保留快速启动 / 恢复开启）<br>② WinSxS 组件存储（DISM 分析 + `StartComponentCleanup`，不加 `/ResetBase`）<br>③ 系统还原点 / VSS 卷影副本瘦身（清理最旧卷影）<br>④ WSL2 / Docker `vhdx` 压缩（`wsl --shutdown` + Windows 虚拟磁盘 API `CompactVirtualDisk`）<br>⑤ 虚拟内存 `pagefile.sys` 状态展示 | 独立大卡片按需操作 | 独立 `deep_slim_execute` | 单次 UAC |

---

## 4. 规则引擎与启发式探测设计

### 4.1 声明式规则 Schema（`builtin.json` + 外置规则包）
- 规则包结构：`RulePack { id, name, version, description, rules: Vec<RuleDef> }`。
- 每条 `RuleDef` 包含：`id`、`name`、`description`、`category`、`defaultSelected`、`admin`、`deleteMode`、`warning`、`processes`、`detectPaths`（可选存在性前提）、`target`（`paths` / `chromiumProfiles` / `firefoxProfiles` / `command` / `recycleBin` / `systemOp`）、`minAgeHours`、`excludeNewestPerDir`、`excludePatterns`、`extensions`。
- **仿火绒外置规则包校验安全红线**：
  - 导入或加载外部 `.json` 规则包时，必须逐条校验：
    1. 不允许 `command` 或 `systemOp` 类型（外部规则包**只能声明文件/目录路径规则**，防止注入任意命令）；
    2. 不允许 `..`、UNC、`\\.\`、`GLOBALROOT`、ADS（`:`）或裸盘根目录（空 `rels`）；
    3. 锚点展开后若命中 `guard::Forbidden` 禁区，执行时会被护栏硬性拦截；
    4. 单包大小上限 2 MiB，规则数上限 500 条。

### 4.2 三大启发式探测器
1. **Chromium / Electron 缓存探测器 (`heuristics::electron`)**：
   - 遍历 `%LOCALAPPDATA%` 和 `%APPDATA%` 深度 1~2 的普通目录（跳过 reparse point、跳过已命中内置规则的目录、跳过禁区与白名单）。
   - 判定签名：目录内存在至少 2 个 Chromium 特征项（如 `Code Cache`、`GPUCache`、`Cache\Cache_Data`、`Crashpad\reports`、`Local State`、`Preferences`、`Network`）。
   - 仅提取安全可再生叶子目录：`Cache`、`Cache\Cache_Data`、`Code Cache`、`GPUCache`、`GrShaderCache`、`ShaderCache`、`DawnCache`、`Crashpad\reports`。绝不触碰 `Local Storage`、`IndexedDB`、`Cookies`、`Session Storage`、`Partitions` 内用户数据。
   - 若命中占用体积 ≥ 1 MB，生成一个独立的 `CleanItem`（名称如 `"{AppDirName} 缓存"`）。
2. **UWP / Microsoft Store 缓存探测器 (`heuristics::uwp`)**：
   - 遍历 `%LOCALAPPDATA%\Packages\*`，仅收集每个包下的 `TempState` 和 `AC\INetCache`（跳过云占位/reparse point）。
   - 聚合为 `"Microsoft Store 应用临时缓存"`（总占用 ≥ 1 MB 时展示）。
3. **卸载残留孤儿目录探测器 (`heuristics::orphans`)**：
   - 构建已安装应用名称与路径集合：读取 HKLM/WOW6432Node/HKCU 下的 `Uninstall` 子键（`DisplayName`、`Publisher`、`InstallLocation`、`UninstallString`），合并运行中进程路径与内置强保护目录白名单（系统/运行时/大型生态厂商根目录至少 80+ 项）。
   - 仅扫描 `%LOCALAPPDATA%` 和 `%APPDATA%` 第一层普通目录：满足“不在保护名单 + 名字与所有已安装程序/厂商无匹配 + 最后修改时间 > 30 天 + 占盘 ≥ 10 MB + 目录内无运行中进程加载模块”。
   - 每个命中目录生成独立 `CleanItem`（`category: leftover`, `defaultSelected: false`, `deleteMode: recycleBin`, 展示路径与最后修改时间）。

---

## 5. 锁文件、进程冲突与安全兜底设计

1. **废除规则级进程一票否决**：扫描阶段无论 Edge/Chrome/QQ/VS Code 是否运行，均正常统计可清理体积，允许用户勾选。
2. **清理前进程冲突检查与优雅关闭**：
   - 点击「立即清理」时，前端先调用 `clean_check_processes(scanId, itemIds)`。
   - 若存在冲突应用，弹出集中提示窗：“以下应用正在运行，关闭后可彻底清理约 X.XX GB”，提供三个按钮：
     - `[关闭应用并清理]` → 调用 `clean_close_processes`，仅对冲突进程的顶层窗口发送 `WM_CLOSE`（绝不调用 `TerminateProcess`），等待最多 4 秒，已关闭则彻底清理，未关闭则降级为单文件跳过；
     - `[跳过占用文件直接清理]` → 直接进入 `clean_execute`，逐文件尽力删除；
     - `[取消]`。
3. **文件级尽力删除 + 可选重启后删除**：
   - `exec/file_cleaner.rs` 遍历目标锚点，所有删除必须通过 `guard::path`（父句柄逐级相对打开 + 禁止 reparse point + 禁区校验 + 拒绝只读属性）执行。
   - 遇到 `GuardError::Locked` / `ERROR_SHARING_VIOLATION` 时，**只跳过该单个文件**并记录到跳过统计，继续清理同一目录下的其他文件。
   - 若用户开启“顽固系统文件在重启后删除”，被锁定的系统垃圾文件（限定在安全系统垃圾锚点内）通过提权 helper 调用 `guard::reboot::schedule_delete_on_reboot`（`MoveFileExW` + `MOVEFILE_DELAY_UNTIL_REBOOT`）写入开机删除队列，并在清理报告中明确列出数量。
4. **单次 UAC 批处理提权**：
   - 若一次「立即清理」包含多个 `requiresAdmin: true` 项（如 `Windows\Temp`、`SoftwareDistribution\Download`、`Windows\Logs`、`DeliveryOptimization`），后端在清理管线的提权阶段把它们合并成**一次** `cclear.exe --elevated --ops ...` 调用，用户只需点一次 UAC。
   - 若用户取消 UAC，管理员项记为“已取消授权（跳过）”，不影响已完成的普通权限项结果。

---

## 6. 双模式空间扫描引擎（并发枚举 + 可选 NTFS `$MFT` 极速扫描）

- **标准模式（默认，免 UAC）**：复用并保留 `crates/cclear-core/src/scan/mod.rs` 的多线程目录枚举（硬链接去重、占盘口径 `AllocationSize`、云占位不触发下载、可取消）。
- **极速模式（NTFS `$MFT`，可选一键开启）**：
  - `win/mft.rs`：打开卷裸句柄 `\\.\C:`，通过 `FSCTL_GET_NTFS_VOLUME_DATA` 读取扇区/簇/MFT 记录尺寸与 `$MFT` 起始 LCN，解析记录 0（`$MFT` 自身）的非驻留 `$DATA` data runs，按 2 MiB 块流式顺序读取所有 MFT 记录，解析 `$FILE_NAME`（0x30）与 `$DATA`（0x80）属性，在内存中一次性构建完整的 `ScanResult`（目录树 + Top 500 大文件）。
  - 若当前进程已是管理员，直接在进程内执行；否则通过 `--elevated --ops mft-scan --result <uuid>.json` 提权执行并把结果写回主进程 `AppState`。
  - 对非 NTFS 卷或解析异常安全降级回标准模式，并向界面返回清晰说明。

---

## 7. 极致精简 3 页前端信息架构（Antigravity 负责）

侧边栏仅保留 3 个核心导航项：

1. **一键清理（默认首页）**：
   - 顶部 **C 盘空间英雄卡**：醒目的 C 盘容量进度条（>85% 警戒红、70–85% 警告橙、<70% 健康绿）、可用/总容量数字、主按钮 `[一键扫描]` / `[立即清理 (预计释放 X.XX GB)]`。
   - **扫描/清理进度态**：实时显示当前正在扫描或清理的项目名 + 累计发现/已释放体积滚动数字 + 取消按钮。
   - **场景分类手风琴树**：5 大分类（系统垃圾、浏览器缓存、常用软件缓存、开发者缓存、卸载残留），分类头带全选/半选复选框与分类小计；子项展示名称、占盘大小、运行中标记、需管理员角标、风险提示、展开查看样例路径/打开目录。
   - **清理完成总结卡**：本次实际释放空间（对比清理前后 C 盘空闲变化）、删除文件数、移入回收站数、跳过数、已排入重启删除数。
   - **系统深度瘦身区（底部大卡片网格）**：休眠文件开关/精简、WinSxS 分析与清理、VSS 卷影瘦身、WSL2/Docker 虚拟盘压缩、虚拟内存状态，每张卡直白写清当前占用 GB 数与操作说明。
2. **空间分析**：
   - 顶部工具栏：扫描目标选择（C 盘 / 用户目录 / 自定义）、`[标准扫描]` + `[MFT 极速扫描 (管理员)]` 切换与扫描进度条。
   - 三视图切换：`目录树` / `占比图 (Treemap)` / `大文件 Top 200`，支持面包屑钻取、打开所在目录，以及**安全移入回收站**（带确认弹窗，后端经禁区护栏校验）。
3. **历史与设置**：
   - **清理历史**：按时间倒序展示每次清理的释放体积、删除/跳过统计、展开查看各条目明细。
   - **白名单（排除保护目录）**：添加/删除不参与扫描和清理的目录路径。
   - **规则库管理（仿火绒）**：显示内置规则包版本与规则数，支持通过本地 JSON 文件导入自定义规则包（显示校验结果）及移除已导入规则包。
   - **清理偏好**：Temp 最小保留时长（24h / 72h / 7 天）、默认是否对系统占用垃圾启用“重启后删除”。

---

## 8. 安全红线（保持 `scripts/check-forbidden-apis.ps1` 100% 通过）

1. **禁止修改 `scripts/check-forbidden-apis.ps1`**。
2. 所有破坏性文件 API（含 `SHFileOperationW`、`MoveFileExW`、`SetFileInformationByHandle`、`DeleteFileW` 等）只能出现在 `crates/cclear-core/src/guard/`。
3. 清空回收站只能在 `crates/cclear-core/src/special/recycle_bin.rs`。
4. 启动外部进程只能在 `crates/cclear-core/src/exec/command.rs` 和 `crates/cclear-core/src/elevation/`。
5. `unsafe` 只能出现在 `win/`、`guard/`、`special/`、`elevation/`，每处必带 `// SAFETY:`。
6. 严禁引入 `tauri-plugin-fs/shell/opener/process/http` 和 `core:default`；所有 Tauri 命令在 4 处同步注册。
7. 所有测试只能在 `tempfile::tempdir()` 运行，绝不触碰真实用户/系统目录。
