# CClear 最终方案（v3，三方两轮评审定稿）

> 来源：plan-v1 → review-r1-{codex,agy} → plan-v2 → review-r2-{codex,agy}。本文是唯一有效版本，之前版本中与本文冲突的内容一律作废。约 10 GB 的“多版本残留”是**待核实的候选体积**，不是已证实的收益。

## 1. 目标与边界
- 个人自用，Windows 11，单用户。桌面 GUI，手动打开使用，没有常驻、托盘、定时任务或服务。
- 能力：系统垃圾、开发缓存、应用缓存、多版本残留、大文件分析和迁移。
- 原则：先分析再动手。按规则分级，每条规则必须有本机验证证据才能获得执行资格。证据不足就降为“仅分析”。任何失败都不会扩大删除范围，也不会提升权限去兜底。
- v1 不开放用户自定义规则，也不开放自定义命令。

## 2. 技术栈（三方一致）
**Tauri 2 + Rust（core crate）+ React/TS**。本机已有 rustc 1.95、VS2022、WebView2、Node 24。
- Rust 负责扫描、规则、计划、执行、journal、路径护栏和提权 helper。前端只负责展示和选择。
- 安全约束：通过 `AppManifest::commands` 把所有自定义命令纳入权限控制。只有主窗口、本地来源能调用执行命令。不启用 fs/shell 插件。前端只传“计划 ID + 选中项 ID”，实际执行清单由 Rust 持有。
- WebView2 使用独立的 `user_data_folder`，并加入自我保护名单。`target/` 放在 G 盘。

## 3. 分级与恢复类型
| 等级 | 含义 | UI |
|---|---|---|
| safe | 可再生，且已有本机验证记录 | 一键清理（执行前展示汇总，可展开） |
| caution | 可再生但有代价，或者是诊断数据 | 逐项勾选 |
| leftover | 有产品适配器证据的多版本残留 | 逐项勾选，默认隔离 |
| manual | 用户数据、程序本体、证据不足的项 | 只分析和引导（打开位置 / 官方卸载 / 迁移 / 送回收站） |
| forbidden | 硬性禁区 | 不可选中 |

`recovery` 取值：`regenerable`（直删）| `irreversible_file`（不可再生文件的永久删除，逐项确认）| `quarantine`（隔离，可还原）| `irreversible_command`（官方命令，单独分组，标明不可撤销和作用范围）。

## 4. 首批规则表（每条只出现一次）
| 规则 | 等级 | recovery | 资格条件 / 备注 | 阶段 |
|---|---|---|---|---|
| Chromium 系 `User Data\<每个 Profile>\{Cache,Code Cache,GPUCache}`（Edge、Chrome） | safe | regenerable | 浏览器进程全部退出；Profile 通过 `Local State` 枚举 | M3a |
| Explorer `thumbcache_*.db` / `iconcache_*.db` | safe | regenerable | 被锁就跳过，不重启 Explorer | M3a |
| Quark：只处理 `Default\{Cache,Code Cache}`、`GrShaderCache` | caution | regenerable | Quark 已退出；专有目录不碰 | M3b |
| 着色器缓存 `D3DSCache`、`NVIDIA\DXCache`、`GLCache`、Chromium `GrShaderCache/ShaderCache` | caution | regenerable | 提示：3D 应用首次启动会变慢 | M3b |
| 用户 Temp，`max(ctime, mtime) ≥ 24h` | caution，默认不勾选 | irreversible_file | 本次未发现占用；只删已批准子树中确认为空的目录 | M3b |
| npm `cache clean --force` / pnpm `store prune` / pip `cache purge` / uv `cache prune`（`clean` 需二次确认） | caution | irreversible_command | 解析出的实际缓存路径必须在 C 盘；显式解析 .cmd/.ps1/.exe；每个命令单独设超时，超时只报告不强杀；失败就跳过 | M3b |
| NuGet `http-cache`、`temp` | caution | irreversible_command | `global-packages` 单列，需二次确认 | M3b |
| Cargo `registry\cache`、`git\db` | caution | regenerable | `cargo`/`rustc` 未运行；不碰 `bin` 和凭据；没装 cargo-cache 就不调用它 | M3b |
| `ms-playwright`、`.cache\puppeteer`、Gradle `caches` | caution | regenerable | 提示可能被项目依赖 | M3b |
| QQ / QQEX 的 `{Cache,Code Cache,GPUCache}` 叶子目录（包括 `users\*\` 下的） | caution | regenerable | 适配器识别版本和布局；所有相关进程退出 | M3b |
| 应用轮转日志：`xwechat\log`、`nt_qq\nt_data\log`、`DingTalk\log`、`office6\log` | caution | irreversible_file | 超过 7 天，并排除每个目录中最新的日志文件 | M3b |
| WER、CrashDumps | caution | irreversible_file | 超过 30 天 | M3b（WER 系统部分在 M5b） |
| C 盘回收站 | caution | irreversible_file | **独立操作，不走冻结清单**：执行前再次显示当前条目数和大小，明确写“清空执行时 C 盘回收站的全部内容” | M3b |
| `codex-runtimes\codex-primary-runtime.previous-*` | leftover | quarantine | `codex.exe`、`codex-code-mode-host.exe` 都已退出；当前 runtime 存在且版本可读 | M4 |
| `codex-runtime-install-*` | leftover | quarantine | 同上，且超过 1 天、未发现占用、晚于当前 runtime 时跳过 | M4 |
| VS Code 非当前 commit 目录、`old_Code.exe` | leftover | quarantine | 读取各 commit 下的 `resources\app\product.json` 和 `Code.exe` 的启动目标；**存在 `updating_version` / `new_Code.exe`（更新进行中）就跳过**；Code 全部退出 | M4 |
| VS Code / Cursor 扩展：`.obsolete` 中列出的目录和 `.<guid>` 临时目录 | leftover | quarantine | 只认 `.obsolete` 和 `extensions.json`；不按“保留最新版”判断；Code/Cursor 全部退出 | M4 |
| MathWorks `ServiceHost` 旧 `v*` | leftover | quarantine | 以 `LatestInstall.info` 为准；`MathWorksServiceHost.exe` 未运行 | M4 |
| `bin\droid.exe.old-*` | leftover | quarantine | 只匹配 droid 的更新命名，当前 `droid.exe` 存在 | M4 |
| updater 安装包：qianfan `installer.exe`、`Temp\DockerDesktopUpdates` | leftover | quarantine | 对应应用未在更新 | M4 |
| LeAppStore `cicada`、NVIDIA App `ota-artifacts` | leftover（需管理员） | quarantine | 提权 helper 内置操作 | M5a |
| `Windows\Temp`、`Windows\Logs`、`$SysReset` | caution（需管理员） | irreversible_file | 内置窄范围操作 | M5b |
| `DISM /StartComponentCleanup`（不做 ResetBase）、Delivery Optimization 官方 cmdlet | caution（需管理员） | irreversible_command | 长耗时任务，有进度显示，不强杀 | M5b |
| WPS `addons\pool` 多版本、`xwechat_files\*\{cache,temp}`、钉钉 `Temp/xcdn/resource_cache`、WPS `download/update`、Chromium `Service Worker`、`extensions_crx_cache` | manual | — | 语义没验证前只分析。验证后可升级（见 §9） | — |
| 未关联的 WSL vhdx | manual | — | 叫“未关联虚拟磁盘”，只展示归属线索，可走大文件迁移 | — |
| HiBit 备份、`.codex` / `.workbuddy-ai` / `.dsh` / `OpenAI\Codex`、模型缓存、`.rustup`、`Local\Programs`、`PowerToys`、`%APPDATA%\npm`、Documents/DeskBox/Downloads、NGR、bilibili/youku/WeMeet/QQMusic、Package Cache | manual | — | 引导官方卸载或应用内清理、迁移 | — |
| WinSxS、DriverStore、`Windows\Installer`、`$PatchCache$`、Program Files*、WindowsApps、`Local\Packages`、OneDrive 根、云占位文件、CClear 自身数据、运行中进程加载的模块文件和独立应用安装根、`db_storage`、`msg`、`nt_db`、`auth`、`security`、`IndexedDB`、`Local Storage`、`office6\backup`、`WPS Cloud Files` | forbidden | — | 用户规则也不能解除 | — |

## 5. 执行引擎（P0 准入条件）
1. **冻结清单**：dry-run 生成的清单（路径、卷 + 文件 ID、大小、时间、规则版本）是执行的唯一输入。有变化的条目跳过，并提示重新扫描。新出现的文件或禁止子项会让对应父目录保留下来。
2. **路径护栏**：
   - 外部输入拒绝 `\\.\`、`GLOBALROOT`、UNC、ADS 和 `..`。内部统一用经过验证的 `\\?\` 长路径。8.3 短名按对象身份归一后再判断。
   - 删除时用父句柄逐级相对打开（`NtCreateFile` + `FILE_FLAG_OPEN_REPARSE_POINT`），在同一个句柄上复核文件身份后按句柄删除。禁止对字符串路径调用 `remove_dir_all`。
   - **M2 准入标准**：竞态测试全部通过（预览后祖先被替换、同名替换、同前缀兄弟目录、硬链接指向范围外、长度超过 260 的合法和越界路径），并且范围外的哨兵文件不变。
3. **占用检测**：用 Restart Manager 按具体文件查询，UI 文案是“本次未发现占用 / 检测不完整”。查询出错不能当作没有占用。执行时再次检查应用状态，发现应用重新启动就停止相关规则。
4. **不做的事**：不接管所有权、不改 ACL、不强杀进程、不停第三方服务。M5 只能通过内置规则，按官方流程管理对应的 Windows 系统服务。
5. **体积口径**：统计 `AllocationSize`，硬链接按“卷 + 文件 ID”去重，拒绝访问的标为“未知”。展示逻辑大小、占盘大小、预计可回收量、实际观测到的空闲变化四个口径。云占位文件只读元数据。

## 6. 隔离区（M4）
- 隔离根目录：在本地固定 NTFS 卷中排除源卷、同步根、源目录的后代，默认选剩余空间最大的卷，执行前展示并允许修改。
- 容量：按最坏情况预留（稀疏、压缩、硬链接展开后的体积），复制过程中持续监测空间。无法保真的对象直接拒绝。
- 事务：预检（拒绝未知的内部 reparse point，处理只读和稀疏属性）→ 复制 → 逐文件哈希和元数据校验 → 确认源没有变化 → 删除源。journal 状态机为 `planned → copied_verified → deleting_source → committed`，重启后先恢复未完成的任务。
- 管理：每个批次可以单独还原或彻底删除。超过 7 天的批次在下次打开时提示，由用户确认后才删除。

## 7. 迁移
- **官方配置迁移（M4）**：uv / npm / pip / NuGet / Gradle。每个工具让用户选“迁移旧缓存”或“允许重建”。记录配置来源 → 写入用户级配置 → 用新进程验证工具能正常工作，并确认生效路径已经改变 → 确认旧位置不再被使用 → 单独确认后清理旧副本。`CARGO_HOME` / `RUSTUP_HOME` 只给指引。
- **junction 迁移（M6，白名单）**：只针对纯数据目录。复用隔离区的事务流程：确认停止写入 → 同卷改名为备份 → 用 DeviceIoControl 创建 junction，目标用短路径 → 改名操作对杀软的临时锁做退避重试 → 用户实测应用正常并确认后才删除备份。迁回时以目标中的最新数据为准，C 盘空间不够就拒绝。

## 8. 里程碑
| 阶段 | 内容 | 准入 / 验收 |
|---|---|---|
| M0 | Tauri 骨架、命令权限控制、core crate、单测框架 | 未授权的窗口或来源无法调用执行命令 |
| M1 | 只读扫描（占盘口径、去重、可取消）、Top N、规则候选展示、扫描完整性展示；锁检测和环境卡（休眠、pagefile 已在 G、WSL 位置、Known Folder 重定向）异步加载 | 不做任何写操作；拒绝访问和未知项可见 |
| M2 | treemap、路径护栏、冻结清单、journal | 通过 §5.2 的竞态测试 |
| M3a | safe 执行 | 只处理冻结清单 |
| M3b | caution 执行（命令、诊断数据、回收站、应用缓存叶子目录） | 每条规则都有本机验证记录 |
| M4 | 隔离事务引擎、leftover、官方配置迁移 | 中断后可恢复；还原经过验证 |
| M5a | 提权 helper + ProgramData 安装包隔离 | 篡改计划、UAC 取消、PATH 劫持等测试不能越权 |
| M5b | Windows\Temp/Logs、DISM、DO | 只在可还原的环境中验证 |
| M6 | junction 迁移和迁回 | 迁回后保留最新数据 |

## 9. 规则准入流程
新规则或从 manual 升级的规则，需要记录：应用版本识别方式、精确路径模板、用途证据（官方文档或实测“删除 → 应用重建 → 功能正常”）、要求关闭的进程、排除路径、recovery 类型。三项缺一项就只能是 manual。

## 10. AI 咨询（T20，用户追加）
- 目的：对拿不准的清理项，让 AI 评估“这是什么、清理后会怎样、是否建议清理”。回答只供参考，不改变分级、勾选或执行资格。
- 联网边界：这是全项目唯一的联网功能。只在用户点击“问 AI”时向用户配置的地址发一次请求；没有后台请求、遥测或自动更新。地址只允许 https，或本机 http（localhost / 127.0.0.1 / [::1]）。禁止重定向。
- 接口：OpenAI 兼容的 Chat Completions（`<地址>/chat/completions`），用户填写地址、模型名、API Key。
- 实现：WinHTTP（`win/http.rs`，复用 `windows` crate，不新增依赖）；API Key 用 DPAPI 加密后存入 `settings.json`（`dpapi:` 前缀），不返回前端、不进日志。
- 隐私：发送内容为规则名称、说明、分级、占用、文件数、锚点和样例路径、判定依据和用户补充的问题；发送前把用户目录替换为 `%USERPROFILE%`、把用户名替换为 `<用户名>`。界面可查看实际发送的内容。
- 前端只传规则 ID 和可选问题；规则信息由后端重新评估得到。
