# CClear —— 个人 C 盘清理工具 初步方案 v1（Droid 起草）

## 1. 目标与约束
- 使用者：只有我自己（Windows 11，单用户 ZhuanZ），手动打开使用，不做常驻、定时或告警。
- 形态：桌面 GUI。
- 四大能力：① 系统垃圾 ② 开发工具缓存 ③ 应用缓存 ④ 大文件分析和迁移到其他盘。
- 安全策略：分级。“安全项”一键清理；“谨慎项”逐项勾选确认；“手动项”只展示、建议和迁移，不自动删除。
- 实测背景见 `docs/scan.md`：可回收的纯垃圾只有约 1–2 GB，大头是开发缓存、应用残留和可迁移目录。

## 2. 技术栈（待三方讨论，Droid 推荐 A）
- A. **Tauri 2 + Rust 后端 + React/TS 前端**：扫描快（并行遍历），安装包小；本机已装 rustup。Rust 负责扫描、规则引擎、删除和迁移；前端负责展示（树图/列表）和确认。
- B. C# .NET 8 + WPF/WinUI 3：调用 Windows API（回收站、Shell、服务、UAC）最顺手，但 UI 生态和 vibe coding 的效率不如 Web 前端。
- C. Python + PySide6/Flet：写得最快，但扫描慢、打包大，而且容易误杀进程。

## 3. 架构
```
UI (React)  ──IPC──>  core (Rust crate, 不依赖 UI，可单测)
                        ├─ scanner     并行目录体积统计（jwalk），跳过 reparse point，硬链接去重
                        ├─ rules       声明式规则（TOML），内置 + 用户自定义
                        ├─ planner     扫描结果 → 清理计划（dry-run 预览）
                        ├─ executor    执行：删除 / 调官方命令 / 隔离区 / 迁移
                        ├─ journal     每次操作写 JSON 日志（可审计、可回滚迁移）
                        └─ elevation   需要管理员的操作交给提权 helper
```

### 3.1 规则（rules/*.toml）示例
```toml
[[rule]]
id = "user-temp"
name = "用户临时文件"
category = "system"          # system | dev | app | large
risk = "safe"                # safe | caution | manual
paths = ["%LOCALAPPDATA%\\Temp"]
min_age_hours = 24            # 只删 24 小时前的文件，避开正在使用的
method = "delete_contents"    # delete_contents | command | quarantine | migrate
admin = false
close_processes = []          # 清理前要求关闭的进程，例如 ["chrome.exe"]

[[rule]]
id = "uv-cache"
category = "dev"
risk = "caution"
method = "command"
command = ["uv", "cache", "clean"]   # 优先用工具官方命令；没装该工具时退回删目录
fallback_paths = ["%LOCALAPPDATA%\\uv\\cache"]
```

### 3.2 首批规则清单
| 分类 | 等级 | 内容 |
|---|---|---|
| 系统 | safe | 用户 Temp（>24h）、Windows\Temp（admin）、缩略图缓存、WER 报告、CrashDumps、回收站、Delivery Optimization 缓存（admin） |
| 系统 | caution | SoftwareDistribution\Download（需先停 wuauserv，admin）、DISM /StartComponentCleanup（admin，耗时长） |
| 开发 | caution | npm/pnpm/yarn、pip、uv、cargo registry/git、NuGet、Gradle/Maven、`~/.cache` 下已知子项、ms-playwright、VS Code CachedData/旧版本扩展、Docker/WSL vhdx 压缩（只给指引） |
| 应用 | caution | Chrome/Edge 的 Cache/Code Cache/GPUCache（只动缓存目录，不动 Profile 数据，需关浏览器）、微信/QQ/钉钉的缓存子目录（只动明确的 cache/temp，聊天文件不碰）、各类 updater 残留安装包 |
| 大文件 | manual | 大目录/大文件排行 + 树图；HiBit Uninstaller 备份、Package Cache 等“疑似残留”只给提示 |
| 永不触碰 | — | WinSxS 直接删除、DriverStore、Windows\Installer（孤立补丁检测风险高，v1 只展示）、pagefile/hiberfil、Program Files、Documents 下的用户数据 |

### 3.3 删除语义
- 关键事实：**放进 C 盘回收站不会释放 C 盘空间**。
- safe 项：直接永久删除（这些是可再生的缓存）。
- caution 项：默认永久删除；可选“隔离区”模式，即先移动到 D:\CClear\quarantine\<时间戳>，保留 N 天后自动清掉，便于后悔。
- 用户自己选的大文件：只允许送回收站（附提示）或迁移，不提供永久删除。
- 全程 dry-run 预览：显示将删除的文件数、大小、路径样例，确认后才执行；被占用的文件跳过并记录。

### 3.4 迁移（大目录搬到 D 盘）
- 优先级 1：用工具自己的配置改位置（例如 `UV_CACHE_DIR`、`npm config set cache`、`PIP_CACHE_DIR`、`RUSTUP_HOME`/`CARGO_HOME`、微信“文件存储位置”）。工具会展示命令或引导，迁移后旧目录清掉。
- 优先级 2：移动目录后创建 **Junction**（`mklink /J`，不需要管理员）。步骤：检查占用进程 → 复制到目标盘 → 校验文件数和大小 → 原目录改名为 .cclear-bak → 创建 junction → 验证可访问 → 删除 .bak。整个过程写入 journal，支持一键回滚。
- 禁止迁移：Windows、Program Files、AppData 根目录本身、正在运行的应用目录。

### 3.5 权限
- 主程序以普通权限运行。需要 admin 的规则汇总成一批，通过一次 UAC 启动 helper（同一个 exe，加 `--elevated-task <plan.json>` 参数）执行，结果写回 journal。

## 4. UI 页面
1. 总览：C 盘使用环形图、可回收空间估算、一键清理安全项按钮。
2. 清理：按分类和等级分组的列表（勾选框、大小、说明、风险提示、需要关闭的进程）。
3. 空间分析：目录树图（treemap）+ Top 文件/目录列表，右键“打开位置 / 迁移 / 送回收站”。
4. 迁移管理：已迁移目录列表，支持回滚。
5. 历史：每次清理释放的空间和日志。

## 5. 里程碑
- M0：Tauri 项目骨架，core crate 和单测框架。
- M1：scanner + 规则加载 + dry-run 预览（只读，零风险）。
- M2：safe 项执行 + journal。
- M3：caution 项（官方命令、关闭进程检测、隔离区）+ 提权 helper。
- M4：空间分析页（treemap、Top N）。
- M5：迁移 + 回滚。

## 6. 测试策略
- core 的单测在临时目录里构造假树（含 junction、只读文件、被占用文件、长路径、Unicode 路径）。
- 执行器有“路径护栏”：在删除前统一校验，目标必须在规则声明的根目录下，不能是盘符根目录或系统黑名单路径，不能跨越 reparse point。这一层单独做穷举测试。
- 真机验证一律先走 dry-run。

## 7. 需要评审重点讨论的问题
1. 技术栈选 A/B/C 中哪一个？理由是什么？
2. 提权模型：按需 UAC 提权 helper，还是干脆整个程序以管理员运行？
3. caution 项默认永久删除还是默认进隔离区？
4. 微信/QQ/钉钉/WPS（Kingsoft）缓存里哪些子目录可以确定安全？
5. Windows\Installer 孤立补丁（8.5 GB）要不要做？如果做，怎么做才足够安全？
6. 迁移：junction 方案有哪些坑（例如应用更新、UWP、OneDrive、杀软）？
7. 还漏掉了哪些高价值的清理项或风险点？
