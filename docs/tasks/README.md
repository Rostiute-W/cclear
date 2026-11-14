# CClear 任务书总览与 agy 执行流程

本目录把 `docs/plan-final.md` 拆成 17 个**按顺序执行**的任务。每个任务由 agy 通过 `codex-dsh-bridge` 技能交给 DSH（DeepSeek）实现，agy 负责审查和合并。Droid 在检查点做整体审查。

## 任务链
| 任务 | 内容 | 里程碑 | 检查点 |
|---|---|---|---|
| T01 | 工程骨架 | M0 | |
| T02 | 扫描器 | M1 | |
| T03 | 扫描接入界面 | M1 | |
| T04 | 规则目录与候选解析 | M1 | |
| T05 | 占用检测、环境卡、只读清理页 | M1 | **A** |
| T05a | 检查点 A 修复 | M1 | **A 复审** |
| T06 | 路径护栏 guard | M2 | |
| T07 | 冻结清单、日志、执行器 | M2 | |
| T08 | 预览确认、历史、treemap | M2 | **B** |
| T09 | 开放 safe/caution 文件规则和回收站 | M3 | |
| T10 | 开发缓存官方命令 | M3 | **C** |
| T10a | 检查点 C 修复 | M3 | **C 复审** |
| T11 | 隔离区事务引擎 | M4 | |
| T12 | 残留适配器 | M4 | |
| T13 | 工具缓存目录官方配置迁移 | M4 | |
| T14 | 提权 helper 与 ProgramData 残留 | M5 | |
| T15 | 系统维护操作 | M5 | |
| T16 | junction 迁移与迁回 | M6 | |
| T17 | README、打包、手工验收清单 | 收尾 | **D** |
| T17a | 检查点 D 修复（上）：隔离区、提权 helper、残留适配器 | 收尾 | |
| T17b | 检查点 D 修复（下）：工具缓存迁移、junction 迁移 | 收尾 | **D 复审** |
| T17c | 检查点 D 复审修复：隔离区性能与收尾问题 | 收尾 | |
| T18 | 界面视觉与应用图标重做（agy 亲手实现） | 收尾 | **E** |
| T18a | 检查点 E 修复：启动恢复、还原临时文件、界面细节（agy 亲手实现） | 收尾 | **E 复审** |

## agy 的执行流程（每个任务都一样）
1. 确认原仓库干净：`git -C G:\cclear status --short` 没有输出。用 `git -C G:\cclear rev-parse HEAD` 取得完整提交号，作为 `baseCommit`。
2. 阅读 `docs/tasks/Txx.md`，按它的“交接字段”调用 `submit_task`（operation 为 execute）：
   - `repoPath`: `G:\cclear`
   - `requestId`: `cclear-Txx-r1`（只有交接内容有改动、需要重新提交时，才改用 r2）
   - `ready`: true
   - `goal`、`allowedPaths`、`validationCommands`、`acceptanceCriteria`、`commitMessage`：从任务文件中**原样复制**
   - `plan` 和 `constraints`：使用下面的通用模板
   - `model`：不填，跟随 DSH 默认
3. 按技能要求，每次 `wait_task` 传 `timeoutMs: 10000`，轮询同一个 taskId，直到 `nextAction` 为 review 或 attention。
4. 进入 `awaiting_review` 后审查：
   - 逐条核对 acceptanceCriteria，并看 worktree 中的真实代码；
   - checks 全部通过；
   - diff 只涉及 allowedPaths；
   - 没有违反 AGENTS.md 的硬性规则（重点是规则 1、3、6、7）；
   - 测试数量与任务书一致，没有冗余测试。
   通过就 approved；不通过就用 changes_requested，并列出具体、可操作的问题。代码风格上的偏好不作为退回理由。
5. 状态变为 `completed` 后合并：`git -C G:\cclear merge --no-ff agent/dsh-<前8位> -m "merge: Txx <标题>"`。合并后只删除该任务 worktree 下的 `node_modules` 目录（为了节省 C 盘空间），不删除 worktree 中的其他内容。
6. 继续下一个任务。
7. **检查点**：T05、T08、T10、T17 合并之后暂停，告诉用户“已到达检查点 X，请通知 Droid 审查”，等用户指示后再继续。
8. **遇到以下情况立即停止，并向用户报告**（报告中写明 taskId 和原因）：
   - `needs_attention`
   - 返工上限用完
   - 任务书存在矛盾或无法实现
   - DSH 中出现与任务书冲突的引导
   停止后不要自己修改任务书、AGENTS.md 或代码，也不要扩大 allowedPaths。

## 通用 plan 模板
```
1. 阅读仓库根目录 AGENTS.md 和 docs/tasks/Txx.md（以及其中引用的 docs/plan-final.md 章节）。
2. 按 Txx.md 的“实现要点”逐条实现，接口命名与任务书一致。
3. 只编写“必须编写的测试”中列出的测试，不添加其他测试。
4. 运行 cargo fmt --all，然后按顺序逐条运行验收命令，直到全部通过。
5. 在最终说明中列出：实现了哪些要点、偏离任务书之处及理由、新增依赖及理由。
```

## 通用 constraints
```
遵守 AGENTS.md 的全部硬性规则；只修改 allowedPaths 内的文件；不做 Txx.md“不要做”中列出的内容；不实现后续任务的功能；不暂存、不提交、不推送；测试和验收命令只能在临时目录中操作，不得触碰真实用户数据；所有 cargo 命令使用 CARGO_TARGET_DIR=G:\cclear-target。
```
