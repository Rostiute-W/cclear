# CClear 方案初稿（plan-v1）第 1 轮独立评审（AGY）

> 评审依据：阅读 [plan-v1.md](file:///G:/cclear/docs/plan-v1.md) 与 [scan.md](file:///G:/cclear/docs/scan.md)，并对本机 C/D/E/F/G 盘、工具链、注册表（WSL/MSI/Uninstall）、进程与大目录结构进行了**只读核验**（未修改任何文件或系统状态）。

## 总评

`plan-v1.md` 整体方向准确（先扫描分析再分级清理、结合迁移而非盲目删文件、core 与 UI 解耦、dry-run 预览 + 路径护栏）。但基于本机实测，原方案存在两类核心偏差：
1. **部分关键路径与机制停留在“通用经验猜测”，与本机实况不符**：例如微信已升级为 4.x（`xwechat` + `xwechat_files`，旧 `WeChat Files` 为 0 MB）；WPS 大头并非普通缓存而是 `addons\pool\win-i386` 里的多版本插件池（2.47 GB）；`AppData\Local\Temp` 中超过 24 小时的文件其实只有 15.7 MB；`AppData\Local\PowerToys` 与 `AppData\Roaming\npm` 是软件本体/全局工具而非缓存。
2. **高价值可回收大头漏抓严重，且安全边界仍有硬伤**：本机实际存在超过 **10 GB** 的“多版本残留 / 安装包缓存 / 孤立虚拟磁盘”（`.cache\codex-runtimes` 冗余 3 GB、VS Code 4 版本并存 2.2+ GB、`ProgramData\Lenovo\LeAppStore\cicada` 1.95 GB、孤立未注册 WSL `ext4.vhdx` 1.40 GB、`MathWorks\ServiceHost` 双版本 1.20 GB、`NVIDIA App\UpdateFramework\ota-artifacts` 884 MB、`~/bin/droid.exe.old-*` 688 MB 等），同时隔离区自动清理与“不常驻”目标冲突、Junction 跨卷/UWP/OneDrive/杀软防护不足。

---

## 一、逐条回答 `plan-v1.md` 第 7 节问题

### 1. 技术栈选 A/B/C 中哪一个？理由是什么？
- **立场**：**明确选 A（Tauri 2 + Rust 后端 + React/TS 前端）**。
- **实机依据与理由**：
  1. **本机工具链已完全就绪**：实测已装 `rustc 1.95.0`（`stable-x86_64-pc-windows-msvc`）、Visual Studio 2022 Enterprise（17.14.35）、`node v24.15.0`、Edge WebView2 Runtime `157.0.4341.46`，零额外重型环境折腾成本。
  2. **并发扫描与底层控制力最强**：面对 `Windows` + `Users` + `ProgramData` 数十万小文件，Rust（`jwalk` 或直接 Win32 `FindFirstFileExW`）无 GC 停顿、内存占用低，且通过 `windows` crate 可原生调用 `GetCompressedFileSizeW`（真实占盘体积）、Restart Manager（精确查锁进程）、`IFileOperation`（回收站）和 `DeviceIoControl`（Junction）。
  3. **UI 表达效率高**：空间分析页的 treemap、分级确认列表、dry-run diff 视图在 React/TS 中开发迭代最快，安装包小且无后台常驻服务。
  4. **排除 B / C 的理由**：选项 B（注：`plan-v1.md` 写的是 `.NET 8`，但**本机实际安装的是 `.NET 9 SDK 9.0.315`**，文档需更正）虽然 Win32/COM 调用最顺手，但 WinUI 3 打包与自定义树图交互体验不如 Web；选项 C（Python `D:\Anaconda3` 3.13.9）扫描大目录性能弱、打包体积大。
- **选 A 的附加工程约束**：
  - Tauri WebView2 需显式指定独立 `user_data_folder` 并加入 CClear 自身扫描/清理排除名单；
  - `target/` 留在 `G:\cclear`，避免构建产物挤爆 C 盘（当前 `C:\Users\ZhuanZ\.cargo` 已有 600 MB，未设 `CARGO_HOME`）。

### 2. 提权模型：按需 UAC 提权 helper，还是干脆整个程序以管理员运行？
- **立场**：**必须采用“主程序普通权限运行 + 按需 UAC 提权 helper”，反对整程序以管理员运行**。
- **理由**：
  1. **UIPI 与 WebView2 兼容性**：管理员身份运行的 GUI 会触发 UIPI，导致普通资源管理器向窗口拖放文件失效，且 WebView2 在提权上下文下的 `UserDataFolder` 隔离容易引发权限异常。
  2. **最小权限就是物理防误删防线**：本机绝大多数高价值清理/迁移目标均在 `C:\Users\ZhuanZ\` 下（71.9 GB），普通权限即可全部操作；普通权限运行可在路径护栏万一失守时由系统 ACL 挡住对 `C:\Windows`、`C:\Program Files` 的误破坏。
  3. **实机权限分布**：普通权限已可读取绝大多数目录（甚至包括 `C:\Windows\Installer`、`C:\Windows\Temp`、`C:\ProgramData\Lenovo\LeAppStore`）；仅 `DeliveryOptimization\Cache`、`AppData\Local\Microsoft\Windows\WER` 等受限目录读取被拒（`DENIED`），以及 `SoftwareDistribution\Download`（停 `wuauserv`）、`ProgramData` 写入、`DISM` 需要提权。
- **关键修正**：
  - 扫描阶段默认只做普通权限快速扫描，提供可选的“管理员深度系统扫描”按钮，避免启动即弹 UAC；
  - 提权 helper 启动后**禁止盲信传入的 `<plan.json>`**，必须独立重跑规则加载与路径护栏校验，且只允许执行白名单内的提权操作（禁止在提权上下文跑 `uv`/`npm` 等依赖用户环境变量的命令）。

### 3. caution 项默认永久删除还是默认进隔离区？
- **立场**：**默认进隔离区（Quarantine），但对“官方命令类”及纯可再生编译缓存支持切换直删，且必须重构当前隔离区机制**。
- **理由与必须修正的 3 个漏洞**：
  1. `caution` 涵盖大量“删错会肉痛或影响功能”的目录（如 `HiBit Uninstaller\Backup` 5.26 GB、`WPS addons`、应用 `Partitions`、孤立 `ext4.vhdx` 1.4 GB），默认进隔离区才能实现“后悔可恢复”。
  2. **漏洞 A（与“不常驻”矛盾）**：`plan-v1.md` 第 1 节承诺“手动打开使用，不做常驻、定时”，3.3 却写“保留 N 天后自动清掉”。应改为**“懒清理（Lazy Purge）”**：每次用户手动打开 CClear 时检测超过 N 天（如默认 7 天）的隔离批次，提示用户确认后手动一键销毁，绝不建后台计划任务。
  3. **漏洞 B（硬编码 `D:\`）**：本机盘符为 `D:`（余 90.7 GB）、`G:`（余 99.7 GB）、`F:`（余 34.6 GB）、`E:`（余 16.4 GB）。隔离区根目录应可配置（默认自动选择剩余空间最大的本地固定 NTFS 盘），并在每次执行前校验目标盘剩余空间 `>= 待隔离体积 + 2 GB`。
  4. **漏洞 C（跨卷移动数万小文件的性能与语义冲突）**：C 盘到 D/G 盘是跨卷移动（复制+删除），对动辄数万小文件的 `npm-cache` / 浏览器缓存极慢；且 `uv cache clean`、`DISM` 这类 `command` 规则本身**不可进隔离区**。因此 UI 应明确区分：`命令清理（直删，不可隔离）`、`进入隔离区（默认）`、`跳过隔离直接永久删除（仅针对高文件数可再生缓存提供开关）`。

### 4. 微信/QQ/钉钉/WPS（Kingsoft）缓存里哪些子目录可以确定安全？
根据本机目录只读核验结果（与旧版经验差异极大），精确拆解如下：

| 应用 | 本机实测分布 | 确定安全可清理子目录（需关闭对应全部进程） | 高风险 / 严禁自动删除 |
|---|---|---|---|
| **微信（WeChat 4.x 新架构）** | `%APPDATA%\Tencent\xwechat`（1,533 MB）+ `%USERPROFILE%\Documents\xwechat_files`（924 MB）；旧 `WeChat Files` 为 0 MB | • `xwechat\log`（**252.1 MB**）<br>• `xwechat\radium\cache`（1.0 MB）、`xwechat\crashpad`<br>• `xwechat\radium\web\profiles\*\Cache`、`Code Cache`、`GPUCache`（仅内部缓存子目录）<br>• `xwechat_files\*\cache`（35.4 MB）、`*\temp`、`*\apm_record`<br>*(若兼容 3.x 仅限 `WeChat Files\*\FileStorage\Cache`、`Temp`、`Log`)* | • **严禁碰**：`xwechat_files\*\msg`（713.8 MB 聊天文件/图视频）、`db_storage`（154.8 MB 数据库）<br>• **勿整删**：`xwechat\xplugin\plugins`（739.2 MB，小程序/OCR/播放器组件）、`radium\web\profiles` 整目录（含登录态） |
| **QQ（QQ NT 9.9）** | `Documents\Tencent Files\*\nt_qq`（688.5 MB）+ `nt_qq\global`（199 MB）+ `%APPDATA%\QQ`（~540 MB） | • `nt_qq\nt_data\log`（**172.2 MB**）、`log-cache`、`nt_temp`<br>• `%APPDATA%\QQ\Crashpad`（22.8 MB）、`QQ\log`（2.3 MB）<br>• `QQ\Partitions\*\Cache`（212.4 MB）、`Code Cache`（83.1 MB）、`GPUCache`、`Dawn*Cache`<br>• `QQ\Cache`、`Code Cache`、`GPUCache` | • **严禁碰**：`nt_qq\nt_db`（276.2 MB）、`QQ\auth`、`QQ\security`<br>• **单列高确认（勿与日志混清）**：`nt_data\Emoji`（195.6 MB）、`Pic`（27.6 MB）<br>• **勿删**：`dynamic_shiply_res`、`dynamic_package` |
| **QQEX（需单列）** | `%APPDATA%\QQEX`（~1.1 GB），含 `global-pad-id.yaml`、`mega-db`、`users\144115210485304026`（954.9 MB） | • `QQEX\Cache`（38.5 MB）、`Code Cache`、`GPUCache`、`weblog`、`logs`、`Crashpad`<br>• `QQEX\users\*\Cache`（**271.3 MB**）、`Code Cache`（**143.0 MB**）、`GPUCache`、`Dawn*Cache` | • **勿整删**：`users\*\Service Worker`（482.9 MB，至多仅清其中 `CacheStorage`）、`IndexedDB`（50.3 MB）、`mega-db`、`miniapp` |
| **钉钉（DingTalk）** | `%APPDATA%\DingTalk`（~1.0 GB） | • `DingTalk\log`（**156.6 MB**）、`holmeslogs`（6.5 MB）、`updaterlogs`、`recordLog`、`Temp`<br>• `DingTalk\*_v3\resource_cache`（11.7 MB）、`image_res_cache`、`image_translate_cache`、`xcdn` | • **严禁碰**：`*_v3\DBFiles`（17.6 MB）、`UserStorage`、`UserKms`、`Sync_v2`、`ImageFiles`/`VideoFiles`<br>• **勿当纯垃圾直删**：`DingpanSyncUpgrade\DocBrowser`（339.3 MB 文档预览器程序）、`*_v3\EAppFiles`（336.5 MB 微应用数据） |
| **WPS（Kingsoft）** | `%APPDATA%\Kingsoft`（**2.95 GB**）；实测 `wpscloudsvr.exe` 常驻后台 | • `office6\cache`（93.9 MB）、`office6\log`（19.5 MB）、`office6\clscache`（1.4 MB）、`office6\update`、`wps\download`<br>• **高价值专清**：`wps\addons\pool\win-i386`（**2.47 GB**）采用**多版本保留去重**（仅保留每插件最新/活跃版本，如 `qing`、`openvino2420x64`、`kdocs` 均保留了 2–3 个旧版，可安全释放 ~0.8–1.2 GB） | • **严禁碰**：`office6\backup`（文档崩溃恢复备份）、`WPS Cloud Files`、`office6\templates`、`office6\dicts`、`personalofflinecache`<br>• **勿整目录清空**：`wps\addons\pool`（会导致 WPS 启动卡顿重下核心组件） |

> 附注：检测“关闭进程”时，WPS 必须同时检查后台驻留进程 `wpscloudsvr.exe`（本机实测即使关闭文档窗口也在运行）以及 `wps.exe`/`et.exe`/`wpp.exe`/`wpspdf.exe`。

### 5. Windows\Installer 孤立补丁（8.5 GB）要不要做？如果做，怎么做才足够安全？
- **立场**：**v1（M0–M5）坚决不做清理，维持“只读展示 + 提示”；若 v2 要做，必须走“提权扫描 + 五重校验 + 强制隔离 ≥30 天”**。
- **本机实测铁证（为何极度危险）**：
  - 本机 `C:\Windows\Installer` 共 **8,721.3 MB**（root `.msi` 332 个 / 7,967.8 MB，`.msp` 22 个 / 313.3 MB，`$PatchCache$` 385.0 MB）。
  - 本次只读测试中，**以非管理员身份**通过 `WindowsInstaller.Installer` COM (`ProductsEx`/`PatchesEx`) + 注册表 `LocalPackage` 比对时，竟得到 **47 个、3,117.4 MB** “未引用 `.msi`”！
  - 但反查 `ProductCode` 与已安装列表后发现，**排名前列的超大包全是在役软件**：`835a8.msi`（528.1 MB，NVIDIA Nsight Compute 2024.1.0）、`83234.msi`（498.5 MB，NVIDIA Nsight VSE 2024.1.0）、`615125.msi`（490.5 MB，NVIDIA Nsight Systems 2026.1.2）、`1a07d4.msi`（65.4 MB，SQL Server 2022）、`10f0bc.msi`（60.8 MB，SOLIDWORKS 2026 SP0）在 `UserData` 中均有匹配（`Matched in UserData=1`）！仅 `2e911d.msi`（387.3 MB，Nsight VSE 2020.2.0）为 `Matched=0`。
  - **结论**：一旦未提权或仅凭 `LocalPackage` 差集判断，就会**误删 >2 GB 的在役重型软件安装数据库**，直接搞坏 SOLIDWORKS / SQL Server / NVIDIA Nsight 的修复、卸载和升级。
- **如果以后要做，最低安全门槛**：
  1. 必须以 **Admin 提权**运行检测（通过 Win32 `MsiEnumProductsExW` / `MsiEnumPatchesExW` 遍历 `MSIINSTALLCONTEXT_ALL` 及 32/64 位注册表视图）；
  2. 必须使用只读 `MsiOpenDatabaseW` 解析疑似孤立文件的 `ProductCode`/`PatchCode`/`UpgradeCode`，**双向反查** `Installer\UserData`、`Classes\Installer` 及 `Uninstall` 键，只要 `ProductCode` 在籍即判为活跃；
  3. 不得整删 `$PatchCache$`（仅可处理已明确卸载产品的对应子目录）；
  4. 只能移入隔离区并保留原文件名与 MSI 摘要元信息（至少 30 天），不提供直删；
  5. 实际真正孤立空间在本机远小于 3 GB，ROI 远不如处理 `HiBit Backup`（5.26 GB）、`codex-runtimes`（~3 GB）、`VS Code 旧版`（2.2+ GB）与 `LeAppStore`（1.95 GB）。

### 6. 迁移：junction 方案有哪些坑（例如应用更新、UWP、OneDrive、杀软）？
Junction（`IO_REPARSE_TAG_MOUNT_POINT`）在本机实测环境下存在 **7 个高发工程坑**：
1. **带自更新器的软件目录迁移必炸**：VS Code、Electron（Squirrel）、`codex-runtimes`、QQ/微信更新器常用“先解压新目录 -> 同卷 `rename` 原子替换”机制。若原目录是 Junction，更新器要么报错 `ERROR_NOT_SAME_DEVICE`，要么直接把 Junction 删掉/改名 `.old`，在 C 盘重新写回实体目录（迁移静默失效并留下双份孤立数据）。**原则：只迁移纯数据/缓存目录，严禁 Junction 迁移含可执行自更新器的程序目录**。
2. **UWP / MSIX 应用（AppContainer 沙盒权限失效）**：本机 `AppData\Local\Packages`（如 `OpenAI.Codex_2p2nqsd0c76g0` 400 MB）依赖 `ALL APPLICATION PACKAGES` 及专属 Package SID ACL。把其子目录 Junction 到 D/G 盘后，目标盘默认继承权限缺乏该 AppContainer SID 访问权，导致应用闪退或写入被拒。**必须把 `AppData\Local\Packages` 列入迁移黑名单**。
3. **OneDrive 同步与 Known Folder 冲突**：本机实测已配置 `OneDrive=C:\Users\ZhuanZ\OneDrive`，`Desktop` 已重定向到 `E:\Desktop`，`Documents` 仍在 `C:\Users\ZhuanZ\Documents`。OneDrive 官方不支持同步目录内出现跨卷 Junction，且遇到云端占位符（`FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`）时移动/复制会触发全量下载或云端同步异常。**迁移前必须禁止 `%OneDrive%*` 路径及含 Cloud Files reparse point 的目录**。
4. **杀软实时防护锁文件（实机装有火绒安全）**：本机运行着 `HipsDaemon` / `HipsTray.exe`（火绒）。迁移步骤中“复制到目标盘 -> 立即将原目录改名 `.cclear-bak` -> 建 Junction”极易被杀软瞬时扫描锁住文件句柄，导致 `rename` 报 `Sharing Violation`。需要：对重命名加退避重试、复制前用 Restart Manager（`RmStartSession`/`RmGetList`）排查所有锁住句柄的进程及服务，而不是只靠人工进程名列表判断。
5. **非 NTFS / 可移动介质风险**：`mklink /J` 仅支持本地固定 NTFS 卷（`DRIVE_FIXED` + `NTFS`），迁往 exFAT/FAT32/网络盘会失败；迁往移动硬盘则拔盘即瘫痪。另外不应通过 `cmd /c mklink /J` 拼命令（防注入/空格/转义问题），应使用 `junction` crate / Win32 `DeviceIoControl`。
6. **普通复制破坏硬链接/稀疏文件/ACL 与 `MAX_PATH` 溢出**：用 `fs::copy` 递归复制会丢失原 ACL/时间戳/稀疏文件属性（如 `.vhdx` 复制后物理体积膨胀），且若原目录内部含硬链接会展开占双倍空间；目标路径建议设计得短而扁平（如 `D:\CClearMig\<short_id>`），防止老程序通过 Junction 访问时超过 260 字符 `MAX_PATH`。
7. **优先级 1（工具官方环境变量）的补充提醒**：本机实测 `UV_CACHE_DIR`、`PIP_CACHE_DIR`、`CARGO_HOME`、`RUSTUP_HOME`、`npm_config_cache` 目前全为空，优先写用户级环境变量 + 迁移旧目录收益极高；但注意 GUI/IDE（如 VS Code、Anaconda）可能需要重启才继承新环境变量，且 `RUSTUP_HOME`/`CARGO_HOME` 改动后需同步确认 `%USERPROFILE%\.cargo\bin` 在 `PATH` 中的引用。

### 7. 还漏掉了哪些高价值的清理项或风险点？
基于本机实测，原方案漏掉了 **至少 10–15 GB 高价值目标** 与多个高危风险：

- **漏掉的高价值清理/去重项**：
  1. **`~/.cache\codex-runtimes` 多版本与失败安装残留（共 4.36 GB，可释放 ~2–3 GB）**：实机包含 `codex-primary-runtime`（1,385 MB），外加 4 个 `codex-runtime-install-*`（合计 ~2.08 GB）和 **8 个 `codex-primary-runtime.previous-*`**（合计 ~0.90 GB）。**绝不可整删 `codex-runtimes`**，但清理 `.previous-*` 及无锁旧安装目录收益极大。
  2. **`AppData\Local\Programs\Microsoft VS Code` 多版本更新残留（共 3.93 GB，可释放 ~2.2+ GB）**：实机同时堆积了 4 个提交哈希目录（`16507d91be`/`2a59476c9b`/`e646868227`/`6d62b1d5da`，各 ~750 MB）以及 `old_Code.exe`、`new_Code.exe`（各 227 MB）。
  3. **`C:\ProgramData` 下的 OEM / 显卡安装包缓存（合计 ~2.84 GB，原方案只写了 `Package Cache`）**：
     - `C:\ProgramData\Lenovo\LeAppStore\cicada`：**1,952.6 MB**（主要为散列命名的 `.exe` 下载安装包）+ `WpEngineCache` 82.8 MB + `logs` 72.6 MB；
     - `C:\ProgramData\NVIDIA Corporation\NVIDIA App\UpdateFramework\ota-artifacts`：**884.0 MB**；
     - （另 `C:\ProgramData\NVIDIA\NGX` 666.5 MB 为 DLSS 模型缓存，可作只读提示）。
  4. **已脱离 WSL 注册表的孤立 `ext4.vhdx`（1.40 GB）**：实机查 `HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss` 显示 `Ubuntu` 与 `docker-desktop` 均已迁到 `G:\WSL\`，但 `AppData\Local\wsl\{2fcfb2b7-42ce-41eb-a7d2-9109cc819908}\ext4.vhdx`（2026-05-25，1,468 MB）仍留在 C 盘。这比“WSL vhdx 压缩指引”直接省 1.4 GB。
  5. **`AppData\Local\MathWorks\ServiceHost` 双版本（2.49 GB，可释放 ~1.28 GB）**：同时存在 `v2026.10.1.1`（1,204.4 MB）、旧版 `v2026.10.0.4`（1,204.1 MB）及 `logs`（83.5 MB）。
  6. **`~/bin/*.old-*` 更新残留（688 MB）**：本机 `C:\Users\ZhuanZ\bin` 存有 2 个 `droid.exe.old-*`（各 343.9 MB）。
  7. **VS Code / Cursor 废弃与多版本扩展（`.vscode\extensions` 2.95 GB）**：`.vscode\extensions\.obsolete` 已记录大量废弃版本未清理，同时存在多版本并存（如 `latex-workshop` 3 版、`jupyter` 2 版、`remote-ssh` 2 版）和 `.5f7ef827-*`/`.06f3ff13-*` 临时失败目录（47 MB）；另外单扩展 `openai.chatgpt-26.1021.22027-win32-x64` 就占 **1.58 GB**（需在分析页高亮）。
  8. **AI 工具生态自身膨胀（合计 > 7 GB）**：`~/.codex`（3.7 GB，含 `.tmp` **380.3 MB**、`logs_2.sqlite` 76.3 MB、`cache` 117.1 MB）、`AppData\Local\OpenAI\Codex`（1.11 GB）、`~/.workbuddy-ai`（1.3 GB，含 `traces` 270.4 MB）、`~/.dsh`（700 MB）。需做细粒度规则（只清明确临时/trace，不动会话数据库）与自我保护（本机 `codex.exe`/`droid.exe` 正在运行）。
  9. **浏览器全覆盖盲区**：Edge（1.6 GB）中 `Default\Service Worker` 占 **301.9 MB**、`extensions_crx_cache` 103.5 MB、`GrShaderCache` + `old_GrShaderCache_000` 23.4 MB；Chrome 另有非默认 `Profile 1`（81.4 MB）；另有 `Quark\User Data`（311.4 MB）。规则不能写死 `Default`，也不能漏掉 `Service Worker\CacheStorage` 与 Shader 缓存。
  10. **娱乐/媒体/临时散落项**：`AppData\Local\Temp\DockerDesktopUpdates`（595.1 MB）、`AppData\Local\NGR\Saved`（649.2 MB）、`AppData\Roaming\bilibili`（~480 MB）、`AppData\Roaming\youku-app`（~370 MB）、`AppData\Roaming\Tencent\WeMeet`（484.2 MB）、`QQMusic`（376.3 MB）、`C:\Windows\Logs`（129.6 MB）、`C:\$SysReset`（71.3 MB）、`C:\EdgeViewCache`（24.7 MB）。

- **漏掉的高危风险点**：
  1. `Temp` 仅按 `mtime` 判断 24h 会误删刚解压的文件；
  2. `SHEmptyRecycleBin` 不指定 `"C:\\"` 会清空全盘回收站；
  3. 把“用户级安装目录”（如 `AppData\Local\PowerToys` 950 MB）或“全局 CLI 包”（`AppData\Roaming\npm` 1.32 GB）误当缓存；
  4. 逻辑文件大小 vs 实际占盘大小偏差（稀疏文件 `.vhdx`、NTFS 压缩、OneDrive 联机占位符）；
  5. 官方命令在 Windows 下执行 `.cmd`/`.ps1`（本机 `npm` 实为 `npm.ps1`）、`PATH` 缺失、弹窗与超时卡死风险。

---

## 二、方案错误、风险与遗漏清单（按 P0 / P1 / P2 排序及修改建议）

### 【P0】必须先修正的阻断性缺陷与高危风险

1. **[P0-1] 隔离区“N 天自动清”与“不常驻”目标冲突，且硬编码 `D:\`、跨卷性能与命令冲突未解**
   - **问题**：`plan-v1.md` 第 1 节禁常驻定时，3.3 却写“保留 N 天后自动清掉”；硬编码 `D:\CClear\quarantine`；未考虑数万小文件跨卷隔离极慢及 `command` 类无法隔离。
   - **修改建议**：
     - 隔离区改为**可配置根路径**（默认自动选剩余空间最大的本地固定 NTFS 盘），执行前校验 `目标盘剩余 >= 待隔离大小 + 2 GB`。
     - 取消自动定时清除，改为**每次手动打开 CClear 时检测超期批次（默认 >7 天）并弹窗/横幅提示手动一键销毁**。
     - 规则增加 `quarantine_supported = true | false`：`method = "command"` 强制不可隔离并明示；高文件数纯编译缓存提供“跳过隔离直删”开关。

2. **[P0-2] `min_age_hours = 24` 仅判修改时间（mtime）会误删新解压安装包，且实机收益与预期不符**
   - **问题**：压缩包解压到 `%LOCALAPPDATA%\Temp` 常保留包内多年前的 `LastWriteTime`；同时实测 `%LOCALAPPDATA%\Temp` 共 781.9 MB 中 `>24h` 仅 **15.7 MB**，`<=24h` 占 766.2 MB（含 595.1 MB `DockerDesktopUpdates`）。
   - **修改建议**：
     - 规则判断文件年龄必须取 **`now - max(CreationTime, LastWriteTime) >= min_age_hours * 3600s`**。
     - UI 将 Temp 拆为“安全可清（>24h）”和“24h 内活跃临时文件（仅展示，不默认清理）”两部分展示，解释差额并防误删进行中的安装。

3. **[P0-3] 回收站清理作用域过大风险（误清 D/E/F/G 盘）**
   - **问题**：`plan-v1.md` 3.2 将“回收站”归为 `safe` 一键清，若直接调用 `SHEmptyRecycleBinW(NULL, NULL, ...)` 会清空所有盘符回收站；且部分用户把回收站当作后悔区。
   - **修改建议**：
     - 代码调用严格限定根路径 **`"C:\\"`**（`SHQueryRecycleBinW` / `SHEmptyRecycleBinW`）；
     - 建议在 `safe` 组中将“清空 C 盘回收站”设为**默认不勾选或加显式确认标签**，防止一键顺手抹掉刚删除待找回的文件。

4. **[P0-4] 迁移方案缺少 UWP/OneDrive/自更新器/火绒锁与原生 API 护栏**
   - **问题**：3.4 的 Junction 步骤在实机有 7 处高发故障（详见第 7 节问题 6）。
   - **修改建议（落到步骤）**：
     1. 扩充**禁止迁移黑名单**：`C:\Windows`、`C:\Program Files*`、`%APPDATA%`/`%LOCALAPPDATA%` 根目录、**`AppData\Local\Packages`**、**`%OneDrive%*`**、**含可执行程序/自更新器的安装目录**（`Local\Programs`、`Local\PowerToys`、`.cache\codex-runtimes`、`.dsh\dsh-runtimes`、`MathWorks\ServiceHost`）。
     2. 迁移前强制检验：**源/目标均为本地固定 NTFS 卷** + **目标盘空间 >= 占盘大小 × 1.2 + 2 GB** + **不含 `IO_REPARSE_TAG_CLOUD` 占位符**。
     3. 用 Win32 **Restart Manager API** 列出全部占用进程与服务；若目标含硬链接或稀疏属性，使用保留属性/稀疏语义的原生复制；`rename -> .cclear-bak` 针对杀软（火绒）瞬时锁定增加退避重试；用 Win32/Rust `junction` crate 在短路径（`D:\CClearMig\<short_id>`）创建，禁止拼接 `cmd /c mklink`。

5. **[P0-5] 扫描体积计算未考虑“物理占盘大小（AllocationSize）”与“云占位符”**
   - **问题**：3 节仅提跳过 reparse point 和硬链接去重。Windows 下稀疏文件（`.vhdx`）和压缩文件逻辑 `len()` 远大于实际占盘空间；OneDrive 联机文件实际本地占 0 字节。
   - **修改建议**：scanner 统一调用 `GetCompressedFileSizeW` / `GetFileInformationByHandleEx(FileStandardInfo)` 累计 **`AllocationSize`**（无权限降级时才退回 `len()` 并标 `*`），并跳过 `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` 属性的云占位符。

6. **[P0-6] 提权 Helper 与普通进程间的信任边界、环境变量隔离漏洞**
   - **问题**：3.5 直接传 `--elevated-task <plan.json>`，未校验路径防篡改；且未禁止在提权 helper 中运行用户态开发命令。
   - **修改建议**：
     - 提权 helper 启动后必须**重新加载内置规则表并全量重跑 Path Guard**，拒绝任何未命中规则根目录或落在黑名单内的路径；
     - `method = "command"`（`uv`/`npm`/`cargo` 等）**严禁交给提权 helper 执行**，仅在当前普通用户进程运行。

---

### 【P1】高价值遗漏补全与关键规则修正

7. **[P1-1] 补齐多版本残留与 AI/OEM 高价值规则（预计新增 10+ GB 净空间）**
   - 具体新增规则建议（均建议归入 `caution` 或 `manual`，且**不可整目录删除**）：
     - **`codex-runtimes-old`**：路径 `%USERPROFILE%\.cache\codex-runtimes`，仅匹配 `codex-primary-runtime.previous-*` 与超时无锁的 `codex-runtime-install-*`（实机省 ~2–3 GB，绝不动 `codex-primary-runtime`）。
     - **`vscode-old-versions`**：路径 `%LOCALAPPDATA%\Programs\Microsoft VS Code`，当 `Code.exe` 全退出时，校验 `updating_version`/当前主版本，清理滞留旧 commit 哈希目录与 `old_Code.exe`（实机 4 版共存占 3.93 GB，省 ~2.2 GB）。
     - **`wps-addons-old-versions`**：路径 `%APPDATA%\Kingsoft\wps\addons\pool\win-i386`，需关闭 `wps.exe`/`wpscloudsvr.exe` 等后按 `<name>_<ver>` 保留最新版本（省 ~0.8–1.2 GB）。
     - **`mathworks-servicehost-old`**：路径 `%LOCALAPPDATA%\MathWorks\ServiceHost`，保留最新版本 `v*` 与日志之外的历史目录（省 ~1.2 GB）。
     - **`user-bin-old-binaries`**：路径 `%USERPROFILE%\bin`，匹配 `*.old-*`（实机 `droid.exe.old-*` 占 688 MB）。
     - **`vscode-obsolete-extensions`**：解析 `%USERPROFILE%\.vscode\extensions\.obsolete`（及 `.cursor\extensions\.obsolete`），清理已标记废弃的扩展目录及 `.guid` 失败临时目录。
     - **`lenovo-leappstore-cache`（admin）**：`C:\ProgramData\Lenovo\LeAppStore\cicada`（1.95 GB 安装包）+ `WpEngineCache`（82.8 MB）+ `logs`（72.6 MB）。
     - **`nvidia-ota-artifacts`（admin）**：`C:\ProgramData\NVIDIA Corporation\NVIDIA App\UpdateFramework\ota-artifacts`（884 MB）。
     - **`wsl-unregistered-vhdx`（manual/caution）**：对比 `HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss` 中的 `BasePath`，标出 `%LOCALAPPDATA%\wsl\*` 下已不在注册表中的孤立 `ext4.vhdx`（实机 1.40 GB）。

8. **[P1-2] 修正微信/QQ/钉钉/WPS 规则路径与进程检测列表**
   - 按上文第一部分第 4 题表格落实路径：`xwechat`（log 252 MB + `xwechat_files\*\cache` 35 MB）、`nt_qq\nt_data\log`（172 MB）+ `QQ\Partitions\*\Cache`（300 MB）、`QQEX` 浏览器子缓存（450+ MB）、`DingTalk\log`（157 MB）+ `resource_cache`、`Kingsoft\office6\cache`（94 MB）+ `log`（20 MB）。
   - WPS 锁检测清单必须包含 `wpscloudsvr.exe`；微信 4.x 需同时覆盖 `Weixin.exe` 与旧版 `WeChat.exe`；QQ 需覆盖 `QQ.exe` 及内置浏览器子进程。

9. **[P1-3] 扩展浏览器缓存匹配规则（多 Profile + Service Worker + Shader）**
   - 将浏览器路径从固定假设改为通配：`User Data\*\Cache`、`*\Code Cache`、`*\GPUCache`、`*\DawnWebGPUCache`、`*\DawnGraphiteCache`、`*\Service Worker\CacheStorage`、`*\Service Worker\ScriptCache`、`User Data\GrShaderCache`、`User Data\old_GrShaderCache_*`、`User Data\BrowserMetrics*`、`User Data\extensions_crx_cache`，覆盖 Edge、Chrome、Quark 等 Chromium 内核应用。

10. **[P1-4] 为 AI 工具目录与程序本体划清白名单/防误杀红线**
    - 明确注释禁止误归为缓存的路径：`%APPDATA%\npm`（全局 CLI）、`%LOCALAPPDATA%\PowerToys`（程序本体）、`%USERPROFILE%\.codex`（除明确 `.tmp` 380 MB / `logs` 之外的会话/包/插件）、`%USERPROFILE%\.workbuddy-ai`（除 `traces` 之外）、`%USERPROFILE%\.dsh`；同时将当前运行中的进程可执行路径纳入保护。

11. **[P1-5] 规范官方清理命令的执行方式**
    - Windows 下 `npm` 入口可能是 `npm.ps1` / `npm.cmd`（实机为 `npm.ps1`），Rust `std::process::Command` 直接调 `"npm"` 可能失败或依赖 shell。应显式查找 `.exe`/`.cmd`、加 `CREATE_NO_WINDOW` 标志隐藏黑框、设置 60 秒超时、校验退出码，失败时不自动盲目回退删目录（改为提示用户选择是否改走删目录）。

---

### 【P2】架构表达力、分析能力与测试/里程碑完善

12. **[P2-1] 扩充 TOML 规则引擎 Schema**
    - 当前 3.1 示例无法表达多版本保留和精确文件过滤。建议在 `[[rule]]` 中新增字段：
      - `min_age_anchor = "max_ctime_mtime"`
      - `include_globs` / `exclude_globs`
      - `version_retention`（按正则分组保留最新 N 个版本，或读取 `.obsolete` / `updating_version` 列表）
      - `services_to_stop`（如 `wuauserv`）
      - `quarantine_mode = "default" | "optional" | "unsupported"`

13. **[P2-2] 补齐娱乐/系统零散项与系统状态只读探测**
    - 在规则或分析提示中补入：`%LOCALAPPDATA%\Temp\DockerDesktopUpdates`（595 MB）、`%LOCALAPPDATA%\NGR\Saved`（649 MB）、`%APPDATA%\bilibili`（~480 MB）、`%APPDATA%\youku-app`（~370 MB）、`%APPDATA%\Tencent\WeMeet\Global\UpdatePackages`（36 MB）、`C:\Windows\Logs`（130 MB）、`C:\$SysReset`（71 MB）、`C:\EdgeViewCache`（25 MB）、`C:\ProgramData\NVIDIA\NGX`（667 MB 提示）。
    - 在总览/分析页增加只读环境状态卡：休眠状态（实机已关闭）、页面文件位置（实机已在 `G:\pagefile.sys`）、CompactOS 状态、Known Folder 重定向（实机 `Desktop -> E:\Desktop`，`Documents -> C:`）、WSL 发行版位置（实机均已在 `G:\WSL\`）。

14. **[P2-3] 修正文档环境笔误并强化测试矩阵与里程碑顺序**
    - 修正 `plan-v1.md` 第 2 节 B 项的 `.NET 8` 为本机实装的 `.NET 9 SDK`；
    - 建议把 **Restart Manager 进程/服务占用检测** 从 M3 前移到 **M1/M2**（dry-run 预览即可标出哪些项当前被锁）；
    - 第 6 节单测补充：**稀疏文件 `AllocationSize` 校验**、**`ctime` 新而 `mtime` 旧边界用例**、**路径护栏大小写/8.3短路径/`\\?\`前缀/`..` 逃逸穷举**、**提权 helper 拒收越权路径负向测试**。
