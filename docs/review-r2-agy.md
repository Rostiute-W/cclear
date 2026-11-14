# CClear 方案 v2 第 2 轮评审（AGY）

> 依据 [plan-v2.md](file:///G:/cclear/docs/plan-v2.md) 与 [review-r1-codex.md](file:///G:/cclear/docs/review-r1-codex.md)，补充针对 v2 新增规则的只读核验后形成本轮意见（不重复已采纳内容）。

## 一、逐条回答 `plan-v2.md` 第 8 节

1. **技术栈裁决 A（附 Codex 的安全约束）**：**同意**。本机实测 `.NET 10 SDK` 确实未安装（仅见 `.NET 9.0.315`），而 `rustc 1.95.0`、VS2022、WebView2 与 Node 均已就绪；保留 Codex 提出的最小 capabilities 与 Rust 侧路径校验约束即可定型。
2. **§2.1 safe 清单是否有不该进一键清理的项**：**有（反对原样执行，需微调两处）**。
   - **反对将显卡/DirectX 着色器缓存（`D3DSCache`、`NVIDIA\DXCache`、`GLCache`）放入 safe**：清理后会导致 SOLIDWORKS 等 3D/GPU 应用首次启动卡顿重编译，这与 §2 自己对 `caution` 的定义（“首次启动变慢”）直接冲突，应移回 `caution`（Chromium `Code Cache`/`GPUCache` 亦可考虑移回 `caution`）。
   - **修正 `Temp >= 7 天` 的口径预期**：实机验证 `max(ctime, mtime) >= 7 天` 为 **0.0 MB**（v2 所引 ~16 MB 实际是 `>=24h` 的结果）；且通用 `Temp` 没有单一“对应进程”，按无锁跳过即可。（另：实测本机 `Explorer\thumbcache_*.db` 约 238 MB 且当前均 `locked=False`，保留在 `safe` 可行。）
3. **§2.3 leftover“当前版本”判定方法是否可靠**：**反对直接按 v2 描述实现，5 项中有 4 项在实机存在误判风险**。
   - **VS Code 本体**：实测 `Code.exe` 的文件版本仅含 `1.143.1`，**不含 commit 哈希**，且当前正处于更新中间态（同时存在 `Code.exe`/`new_Code.exe`/`old_Code.exe`/`updating_version` 和 `bin\new_code.cmd`）。必须读取 `<commit>\resources\app\product.json` 并检查 `updating_version`；若存在未完成更新标记，应降级 `manual`，不得盲删。
   - **VS Code/Cursor 扩展**：实测 `.vscode\extensions\extensions.json` 同时登记了多版本的 `jupyter`、`latex-workshop`、`remote-ssh`、`pull-request-github`，它们**不在 `.obsolete` 中**。仅按目录名“保留最新版”会破坏 `extensions.json` 或误删多配置/远程环境依赖；应**只清理 `.obsolete` 命中项与 `.<guid>` 临时目录**，其余多版本归 `manual`。
   - **MathWorks `ServiceHost`**：同级已有权威清单 `LatestInstall.info`（`LatestDSInstallRoot`），不应靠字符串比大小，且须检查 `MathWorksServiceHost.exe` 占用。
   - **WPS `addons\pool` 与 `codex-runtimes`**：WPS 存在 `3.1.0.457` 与 `3.1.0.1563`，必须按数值 SemVer 段排序并结合 `addons\listV3\win-i386\<WPS版本>`；`codex-runtimes` 实机 `codex-runtime-install-3izKSm` 更新时间（11/10）晚于 `codex-primary-runtime`（11/05），必须要求 `codex.exe` 与 `codex-code-mode-host.exe` 全退出后再按“>1 天且无锁”清理。
4. **有没有 v2 新引入的问题，或第 1 轮 P0 仍未解决**：**有**（3 条 P0 与 4 条 P1，见第二节）。
5. **里程碑顺序和 M1 范围是否合理**：**部分同意，建议微调两处**。M1 只读范围合理；但**反对将 treemap 推迟到最后的 M6**——treemap 是纯只读前端视图，前移到 **M2** 才能在动手清理前看清空间分布；另外 `LeAppStore\cicada`（1.95 GB）与 `NVIDIA OTA`（884 MB）属纯文件隔离，M5 应与长耗时维护命令 `DISM` 解耦，优先交付。

---

## 二、v2 残留与新引入的问题清单（按 P0/P1/P2）

### P0（必须在对应阶段实现前修正）

1. **[P0-1 新引入] §5 路径护栏“拒绝 `\\?\`”与长路径支持、Rust `canonicalize()` 互斥**
   - **问题**：Rust `fs::canonicalize()` 在 Windows 默认返回 `\\?\C:\...`，且 Win32 读写 >260 字符路径必须依赖 `\\?\`。一刀切拒绝会令深层目录全部报错。
   - **具体修改**：护栏改为**拒绝外部原始输入中的 `\\.\` 设备路径、`GLOBALROOT`、UNC 共享、ADS（`:`）与 `..`**，通过组件归一化与 8.3 短名消除后，**内核内部统一使用 `\\?\` 或基于父目录句柄（`FILE_FLAG_OPEN_REPARSE_POINT`）做相对操作**。

2. **[P0-2 新引入] §2.5 `forbidden`“运行中进程 exe 所在目录”封杀 `~/bin` 清理并产生边界冲突**
   - **问题**：本机 `droid.exe` 正在 `C:\Users\ZhuanZ\bin` 运行，按“exe 所在目录整体 forbidden”，会导致同目录 `droid.exe.old-*`（688 MB）永远不可选中；同时 §5“不停用户服务”与 M5 管理 `wuauserv` 表述不严。
   - **具体修改**：`forbidden` 中的运行进程保护应精确到**运行中进程锁定的 `.exe`/`.dll` 模块文件本身，以及独立应用安装根目录（如 `Local\Programs\*`）**，不对 `~/bin` 这类通用工具集合目录整目录加禁；§5 明确改为“不擅停第三方用户态服务，M5 仅允许内置系统规则经 UAC 按官方流程管理对应 Windows 系统服务”。

3. **[P0-3 未闭环] §2.3 leftover“当前版本”判定失真（见上文 Q3）及 §3 隔离事务缺失细则**
   - **问题**：VS Code 与扩展的“当前版本”判定会误删正常索引/破坏升级中状态；§3 对 `quarantine` 仅写“复制 → 哈希校验 → 删源”，未继承 §6 对**稀疏文件（1.4 GB `ext4.vhdx`）、内部 reparse point、只读位、以及删源阶段崩溃恢复**的约束。
   - **具体修改**：leftover 判定按上文第 3 题逐项改为读取 `product.json`+`updating_version`、仅清 `.obsolete`/`.<guid>`、读取 `LatestInstall.info`、SemVer 数值比较+停进程；§3 隔离引擎复用 §6 预检（拒绝未知内部重解析点，显式处理稀疏/只读属性），并在 journal 中记录 `copied_verified -> deleting_source -> committed` 状态机以便重启续处理。

### P1（影响可用性与实际清理收益）

4. **[P1-1 新引入] §2.2 `cargo cache` 并非官方子命令，且禁止 fallback 使 Cargo 448 MB 缓存彻底不可清**
   - **问题**：本机实测 `cargo cache --version` 直接报错 `no such command: cache`（它属于第三方 crate `cargo-cache`）。按 v2“失败即跳过”，Cargo 缓存等于失效。
   - **具体修改**：探测是否已安装 `cargo-cache.exe`；未安装时不要调用不存在命令，改为对 `%USERPROFILE%\.cargo\registry\cache` 与 `git\db` 提供**只清明确缓存子目录**的可选规则（严禁碰 `.cargo\bin` 与凭据，且须在 `cargo`/`rustc` 未锁定时执行）。

5. **[P1-2 新引入] §2.2 应用日志限定“30 天前”使实机 600 MB 日志缩水为 87.6 MB**
   - **问题**：实测 `xwechat\log`（252.1 MB）中 `>=30 天` 仅 1.0 MB、`nt_qq\nt_data\log`（172.2 MB）为 0 MB、`office6\log`（19.5 MB）为 0 MB、`DingTalk\log`（156.6 MB）为 86.6 MB，因聊天工具日志按短周期轮转。
   - **具体修改**：将应用轮转日志的保留期由 30 天调整为 **7 天**（并排除当日最新日志文件）；`WER` / `CrashDumps` 维持 30 天。

6. **[P1-3 新引入] §2.1 着色器缓存错放 `safe` 与 `Temp >= 7 天` 体积标注失实**
   - **具体修改**：`D3DSCache`、`NVIDIA\DXCache`、`GLCache` 移至 `caution`；`Temp >= 7 天` 修正预期说明（实机 0 MB），同时可考虑将 `Temp >= 24h`（15.7 MB，同样按 `max(ctime, mtime)` + 无锁）纳入 `caution` 供手动勾选。

7. **[P1-4 遗漏] §2.3 多项 leftover 缺少前置进程条件，且 `Quark` 非标准布局需收窄**
   - **具体修改**：`codex-runtimes` 规则显式绑定退出 `codex.exe`/`codex-code-mode-host.exe`；`.vscode\extensions` 清理 `.obsolete` 须退出 `Code.exe`；`Quark\User Data` 下存在 `pars_data`/`persistence`/`unet` 等专有目录，应从通用 `*` 通配中收窄，仅对其 `Default\Cache`、`Default\Code Cache`、`GrShaderCache` 生效并核对 Quark 进程退出。

### P2（体验与演进优化）

8. **[P2-1] 里程碑顺序微调**：将只读 treemap 从 M6 前移至 **M2**；M5 拆分为“M5a：`ProgramData` 安装包（`LeAppStore\cicada`、`NVIDIA OTA`）提权隔离”与“M5b：`DISM`/`DO` 系统维护”。
9. **[P2-2] 补齐 M4 隔离区单批清理/还原入口与系统零散项归位**：M4 UI 需同时支持“单批一键还原”与“单批手动彻底删除”（不能只等 7 天后提醒）；明确将 `C:\$SysReset`（71.3 MB）、`C:\Windows\Logs`（129.6 MB）列入 M5 窄范围维护候选或 `manual` 展示，防止被 `C:\Windows` 禁区完全遮蔽。
