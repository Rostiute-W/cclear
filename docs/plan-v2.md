# CClear 方案 v2（Droid 整合，基于 review-r1-codex.md 与 review-r1-agy.md）

> 细节依据见两份评审，本文只记录决策和改动。v1 中未被推翻的内容继续有效，除非本文另有说明。

## 0. 第 1 轮分歧与裁决
| 议题 | Codex | AGY | v2 裁决 |
|---|---|---|---|
| 技术栈 | WPF + .NET 10 LTS | Tauri 2 + Rust + React/TS | **暂定 A：Tauri 2 + Rust**。理由：本机已核实 VS2022、WebView2、rustc 1.95 都已就绪，而 .NET 10 SDK 还没装（.NET 8/9 已于 2026-11-10 停止支持）；空间分析页（树图、大列表）用 Web 实现效率高；Rust 的 `windows` crate 能覆盖 Restart Manager、IFileOperation、GetCompressedFileSizeW、文件 ID 等需求。同时采纳 Codex 的约束：Tauri capabilities 只开放最少的自定义命令；前端不能直接接触文件系统或 shell 插件；所有路径判断只在 Rust 层做。请 Codex 在第 2 轮确认，或给出足以推翻这个结论的证据 |
| 提权 | 按需 helper，强约束 | 按需 helper，强约束 | 一致。helper 只接受内置操作 ID 和受限参数，自己重新解析规则、重新运行路径护栏；开发工具命令绝不提权执行；用户目录按调用者 SID 解析 |
| caution 默认动作 | 支持文件级恢复的就隔离；官方命令单列为“不可撤销” | 默认隔离；可再生缓存可以直删；懒清理 | 见 §3：**按 recovery 类型决定**。可再生缓存在确认后直删；不可再生内容默认隔离；官方命令单列为“不可撤销” |
| 应用缓存 | 不进一键清理，只给精确叶子路径候选 | 给出本机精确路径表 | 采用 AGY 的路径表作为 **caution 规则**。规则绑定应用版本识别，布局不匹配时降级为“仅分析”。不进 safe |
| Windows\Installer | 不做 | v1 不做（已实测发现误判 >2 GB 在役包） | 一致：**只展示，永不清理**。Package Cache 同样处理 |
| junction | 高级人工操作，放在最后，要求完整事务 | 7 个坑和黑名单 | 合并为 §5 |

## 1. 范围调整
- **先交付只读分析的价值**：M1 就具备 Top N、候选规则展示、锁占用展示和环境状态卡。
- **v1 的 safe 层缩小到“已验证可再生 + 未被占用”**（§2）。回收站、WER、CrashDumps 和通用 Temp 新文件全部移出 safe。
- **新增“多版本残留”分类（`leftover`）**，这是本机收益最大的一类（约 10 GB）。

## 2. 分级定义（v2）
| 等级 | 含义 | 执行方式 |
|---|---|---|
| safe | 可再生缓存，规则已在本机验证，相关进程未运行，文件未被锁 | 一键清理（仍会先展示汇总清单，可展开）；直接删除 |
| caution | 可再生但有代价（重新下载、重新登录、首次启动变慢），或者是诊断数据 | 逐项勾选；可再生的直删，诊断数据按保留期处理 |
| leftover | 多版本残留、失败的安装目录、孤立的 vhdx、updater 安装包 | 逐项勾选；**默认进隔离区** |
| manual | 用户数据、大文件、程序本体、需要官方卸载或迁移的目录 | 只展示和引导：打开位置、官方卸载、官方配置迁移、junction 迁移、送回收站 |
| forbidden | 硬性禁区，用户规则也不能解除 | 不可选中 |

### 2.1 首批 safe（全部要求对应进程已退出，并通过 Restart Manager 确认无锁）
- `%LOCALAPPDATA%\Temp` 中 `now - max(CreationTime, LastWriteTime) >= 7 天` 的文件，空目录自底向上清理（本机 >24h 的只有约 16 MB，所以 safe 层本身收益很小。这是预期内的，用户能看到原因）
- Chromium 系（Edge/Chrome/Quark 及所有 Profile）：`User Data\*\{Cache,Code Cache,GPUCache,DawnWebGPUCache,DawnGraphiteCache}`、`User Data\{GrShaderCache,ShaderCache}`
- DirectX/NVIDIA 着色器缓存：`%LOCALAPPDATA%\D3DSCache`、`%LOCALAPPDATA%\NVIDIA\DXCache`、`GLCache`
- 缩略图和图标缓存 `%LOCALAPPDATA%\Microsoft\Windows\Explorer\{thumbcache,iconcache}_*.db`（被 Explorer 锁住时跳过，不重启 Explorer）

### 2.2 首批 caution
- 开发缓存（**只用官方命令，失败就跳过，不退回删目录**）：`npm cache clean --force`、`pnpm store prune`、`pip cache purge`、`uv cache prune`（默认）/ `uv cache clean`（需二次确认）、`cargo cache`（没装就只展示）、`dotnet nuget locals all --clear`。命令绑定解析出的实际缓存路径，执行方式按 AGY P1-5（显式解析 .cmd/.ps1/.exe、CREATE_NO_WINDOW、设超时、检查退出码）
- 删目录型开发缓存（官方明确可删）：`ms-playwright` 和 `.cache\puppeteer`（需提示可能被项目依赖）、Gradle `caches`、VS Code `CachedData` 中非当前版本的子目录
- Chromium `Service Worker\{CacheStorage,ScriptCache}`、`extensions_crx_cache`
- 应用缓存：按 review-r1-agy §4 的表，只做精确叶子路径。进程清单：WPS 要包含 `wpscloudsvr.exe`，微信要包含 `Weixin.exe` 和 `WeChat.exe`，QQ/QQEX 要包含子进程
- 诊断数据：WER、CrashDumps、`xwechat\log`、`nt_qq\nt_data\log`、`DingTalk\log`、`office6\log`。只删 30 天前的
- C 盘回收站：只清 `C:\`，显示条目，不勾选为默认

### 2.3 首批 leftover（默认隔离）
- `.cache\codex-runtimes\codex-primary-runtime.previous-*`，以及无锁且超过 1 天的 `codex-runtime-install-*`（绝不动 `codex-primary-runtime`）
- VS Code `Programs\Microsoft VS Code` 中非当前 commit 的目录和 `old_Code.exe`（当前版本从 `Code.exe` 的版本资源 / `product.json` 判断；Code 必须全部退出）
- `.vscode\extensions` 和 `.cursor\extensions`：`.obsolete` 中列出的目录和 `.<guid>` 失败目录；同一扩展有多个版本时保留最新版
- WPS `addons\pool\win-i386` 每个插件保留最新版本
- `MathWorks\ServiceHost` 保留最新的 `v*`
- `%USERPROFILE%\bin\*.old-*`
- 孤立 WSL vhdx：`%LOCALAPPDATA%\wsl\*\ext4.vhdx` 且不在 `HKCU\...\Lxss\*\BasePath` 中（另外要求 `wsl --list` 不在运行）
- updater 安装包：`%LOCALAPPDATA%\qianfan-desktop-app-updater\installer.exe`、`Temp\DockerDesktopUpdates`（Docker 未在更新时）
- 需管理员（M5 再做）：`ProgramData\Lenovo\LeAppStore\cicada`、`ProgramData\NVIDIA Corporation\NVIDIA App\UpdateFramework\ota-artifacts`

### 2.4 manual（只分析和引导）
HiBit Uninstaller 备份（5.2 GB，用 HiBit 自身管理或手动迁移）、`.codex` / `.workbuddy-ai` / `.dsh` / `OpenAI\Codex`（只把明确的 `.tmp`、`traces` 列为 caution 候选，其他保护）、`.cache\huggingface` 等模型缓存、`.rustup` 工具链（引导 `rustup toolchain list/uninstall`）、`Local\Programs`、`PowerToys`、`%APPDATA%\npm`（全局包）、Documents / DeskBox / Downloads 大文件、NGR\Saved、bilibili/youku/WeMeet/QQMusic（引导应用内清理）、Package Cache。

### 2.5 forbidden
`C:\Windows` 整体（Windows\Temp、DISM、DO 等只能以 M5 的窄范围内置操作出现）、WinSxS、DriverStore、Installer、`$PatchCache$`、Program Files*、WindowsApps、`AppData\Local\Packages`（不能迁移，也不能删）、OneDrive 根、云占位文件、CClear 自身数据和 WebView2 目录、当前运行中进程的可执行文件所在目录、`*\db_storage`、`*\msg`、`nt_db`、`auth`、`security`、`IndexedDB`、`Local Storage`、`office6\backup`、`WPS Cloud Files`。

## 3. 删除和恢复语义
- 每条规则都声明 `recovery = regenerable | quarantine | irreversible_command`。
- **regenerable**：确认后直接删除。
- **quarantine**：先走“复制 → 逐文件哈希校验 → 删源”的事务，移到隔离根目录。隔离根目录可配置，默认选剩余空间最大的本地固定 NTFS 卷，要求剩余空间 ≥ 待隔离量 + 2 GB。不满足条件时停止该项，**不退回永久删除**。
- **到期处理**：每次手动打开 CClear 时，提示超过 7 天的批次，由用户确认后清掉。没有后台任务。
- **irreversible_command**：UI 单独分组，标明不可撤销，并说明作用范围和无法逐文件预览的限制。
- **执行对象冻结**：dry-run 生成的清单（路径、卷 + 文件 ID、大小、时间、规则版本）就是执行的唯一输入。发现变化的条目跳过，并提示重新扫描。

## 4. 扫描和体积口径（采纳双方的 P0/P1）
- 按 `AllocationSize`（GetCompressedFileSizeW / FileStandardInfo）统计，同时展示逻辑大小。硬链接按“卷 + 文件 ID”去重。拒绝访问的目录标为“未知”，不算作 0。
- 云占位文件只读元数据，不触发下载。不跟随 reparse point。
- 展示四个口径：逻辑大小、占盘大小、预计可回收量、执行后实际观测到的空闲空间变化。
- 顺序：先扫规则候选和用户目录，再按需扫全盘。并行度有限，可取消，有进度显示。

## 5. 路径护栏（P0，独立模块，单独测试）
- 按路径组件判断归属，不用字符串前缀。校验最终卷和文件 ID、所有祖先都不是 reparse point。拒绝 `\\?\`、设备路径、ADS、8.3 短名绕过和 `..`。
- 执行时用句柄打开父目录，再相对它删除（`FILE_FLAG_OPEN_REPARSE_POINT` + 对句柄重新校验），防止检查之后路径被替换。
- 不接管所有权、不改 ACL、不强杀进程、不停用户服务。
- 测试要求：规则范围外的哨兵文件内容和元数据都不变；覆盖同前缀兄弟目录、预览后祖先被替换、硬链接指向范围外、执行前同名替换等用例。

## 6. 迁移（M4 官方配置迁移，M6 junction）
- **优先用官方配置**：uv / npm / pip / NuGet / Gradle 的缓存目录变量（本机都还没设置）。流程：记录旧配置 → 写入用户级配置 → 用新进程验证生效路径 → 提示需要重启 IDE 和终端 → 用户确认后才清旧目录。`CARGO_HOME` / `RUSTUP_HOME` 只给专门指引，不做自动迁移。
- **junction（M6，白名单制）**：只针对纯数据目录。按 Codex §6 的事务流程执行：预检 → 复制到临时目录 → 逐文件哈希校验 → 确认无写入者（Restart Manager）→ 源目录同卷改名为备份 → 用 DeviceIoControl 创建 junction（不拼接 `cmd mklink`），目标用短路径，例如 `<盘>:\CClearMig\<id>`。改名操作对火绒等杀软的临时锁做退避重试。备份要在应用实测通过、用户确认后才删除。迁回时以目标里的最新数据为准，C 盘空间不够就拒绝迁回。
- 迁移黑名单：AGY P0-4 的清单，加上 forbidden 中的所有内容，再加含自更新器的程序目录。

## 7. 里程碑（v2）
| 阶段 | 内容 |
|---|---|
| M0 | Tauri 骨架：最小 capabilities，`target` 放在 G 盘，core crate 和单测框架 |
| M1 | 只读：scanner（占盘口径、去重）、Top N 列表、规则候选展示、Restart Manager 锁展示、环境状态卡（休眠、pagefile 已在 G、WSL 位置、Known Folder 重定向） |
| M2 | 路径护栏 + journal + 冻结清单 + 执行 safe 层 |
| M3 | caution 层（官方命令、应用缓存、诊断数据、C 盘回收站） |
| M4 | 复制-校验事务引擎 → 隔离区 → leftover 层 → 官方配置迁移 |
| M5 | 提权 helper：Windows\Temp、LeAppStore、NVIDIA OTA、`DISM /StartComponentCleanup`（不做 ResetBase）、DO 官方 cmdlet |
| M6 | junction 迁移 + 迁回；treemap |

## 8. 第 2 轮请重点确认
1. 技术栈裁决 A（附 Codex 的安全约束）是否接受？不接受就请给出决定性理由。
2. §2.1 safe 清单里有没有仍然不该进一键清理的项？
3. §2.3 leftover 规则的“当前版本”判定方法是否可靠？
4. 有没有 v2 引入的新问题，或者第 1 轮的 P0 还没解决？
5. 里程碑的顺序和 M1 的范围是否合理？
