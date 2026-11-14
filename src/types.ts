// 与 Rust 端跨 IPC 的结构体一一对应，字段名保持 camelCase。
export interface AppInfo {
  version: string;
  isAdmin: boolean;
}

export interface VolumeInfo {
  root: string;
  label: string;
  fs: string;
  total: number;
  free: number;
  isSystem: boolean;
}

export interface ScanRoot {
  label: string;
  path: string;
}

export interface ScanProgress {
  dirs: number;
  files: number;
  allocated: number;
  elapsedMs: number;
  done: boolean;
  cancelled: boolean;
  error: string | null;
}

export interface ScanSummary {
  root: string;
  logical: number;
  allocated: number;
  files: number;
  dirs: number;
  deniedDirs: number;
  elapsedMs: number;
  cancelled: boolean;
}

export interface DirNode {
  id: number;
  name: string;
  logical: number;
  allocated: number;
  files: number;
  denied: boolean;
  reparseSkipped: number;
  hasChildren: boolean;
  percentOfParent: number;
}

export interface FileEntry {
  index: number;
  path: string;
  logical: number;
  allocated: number;
  lastWrite: number;
}

// 与 Rust 端 model.rs 的枚举一致，取值由 serde 转成 camelCase。
export type Tier = "safe" | "caution" | "leftover" | "manual" | "forbidden";

export type Recovery =
  | "regenerable"
  | "irreversibleFile"
  | "quarantine"
  | "irreversibleCommand"
  | "none";

export type Category = "system" | "browser" | "dev" | "app" | "leftover" | "userData" | "program";

export type Phase = "analysis" | "m3a" | "m3b" | "m4" | "m5a" | "m5b";

export type TargetKind = "paths" | "chromiumProfiles" | "adapter" | "command" | "special" | "system";

export type CandidateStatusKind =
  | "ready"
  | "notPresent"
  | "processRunning"
  | "layoutMismatch"
  | "phaseLocked"
  | "forbidden"
  | "insufficient";

export interface RuleInfo {
  id: string;
  name: string;
  description: string;
  category: Category;
  tier: Tier;
  recovery: Recovery;
  phase: Phase;
  admin: boolean;
  defaultSelected: boolean;
  processes: string[];
  warning: string | null;
  targetKind: TargetKind;
  // 命令类规则的工具信息，其他规则为 null。
  command: CommandInfo | null;
  // 系统操作规则对应的内置操作名；还没有内置操作时为 null。
  systemOp: string | null;
}

// 命令类规则调用的工具和二次确认要求，确认文案按它写。
export interface CommandInfo {
  tool: string;
  needsSecondConfirm: boolean;
}

export interface CandidateInfo {
  ruleId: string;
  status: CandidateStatusKind;
  statusDetail: string | null;
  anchors: string[];
  logical: number;
  allocated: number;
  files: number;
  samples: string[];
  // 判定依据，一条一个条目。残留规则靠它说明为什么选中这些目录。
  evidence: string[];
  // 没有选中某些项的原因。
  notes: string[];
  executable: boolean;
}

// 占用检测结果。complete 为 false 表示还有文件没查过，不能当成“没有占用”。
export interface LockReport {  checked: number;
  total: number;
  complete: boolean;
  lockers: LockerInfo[];
  error: string | null;
}

export interface LockerInfo {
  pid: number;
  appName: string;
  service: string | null;
}

// 环境状态卡里的一个位置项。path 为 null 表示读不到，界面显示“未知”。
export interface EnvLocation {
  // 显示名。页面文件和 OneDrive 这类靠分组标题说明的项为 null。
  name: string | null;
  path: string | null;
  // true 表示在 C 盘，false 表示已在其他盘，null 表示判断不了。
  onSystemDrive: boolean | null;
  // 文件大小。只有按文件展示的项（未关联虚拟磁盘）才有。
  sizeBytes: number | null;
}

export interface EnvState {
  hibernateEnabled: boolean | null;
  pagefiles: EnvLocation[] | null;
  wslDistros: EnvLocation[] | null;
  // 不属于任何已注册发行版的 WSL 虚拟磁盘，只展示。
  unregisteredVhdx: EnvLocation[] | null;
  knownFolders: EnvLocation[];
  onedrive: EnvLocation | null;
}

// 冻结清单里一条规则的概要。清单是执行的唯一输入，执行时逐项复核。
export interface PlanRuleSummary {
  ruleId: string;
  recovery: Recovery;
  anchors: string[];
  items: number;
  logical: number;
  allocated: number;
  files: number;
  dirs: number;
  skippedAtBuild: number;
  // 构建清单时被跳过的条目，展开可以看具体原因。
  skippedDetails: PlanSkippedBuild[];
}

export interface PlanSkippedBuild {
  path: string;
  reason: string;
}

export interface PlanSummary {
  planId: string;
  // Unix 秒，界面按本地时区格式化。
  createdAt: number;
  rules: PlanRuleSummary[];
}

export interface PlanItem {
  itemId: number;
  ruleId: string;
  path: string;
  isDir: boolean;
  allocated: number;
  // Unix 秒，0 表示时间不可用。
  lastWrite: number;
}

export interface ExecProgress {
  ruleId: string;
  deletedFiles: number;
  deletedDirs: number;
  freedAllocated: number;
  skipped: number;
  done: boolean;
}

export interface ExecRuleReport {
  ruleId: string;
  deletedFiles: number;
  deletedDirs: number;
  freedAllocated: number;
  skipped: number;
}

export interface ExecSkip {
  ruleId: string;
  path: string;
  reason: string;
}

export interface ExecReport {
  rules: ExecRuleReport[];
  skipped: ExecSkip[];
  skippedTotal: number;
  freeBefore: number;
  freeAfter: number;
  cancelled: boolean;
  // 中止原因，null 表示正常跑完。
  aborted: string | null;
}

export interface PlanHistoryEntry {
  planId: string;
  startedAt: number;
  // 这次执行涉及的规则 ID，名称由界面查规则目录。
  rules: string[];
  freeBefore: number;
  freeAfter: number;
  deletedFiles: number;
  deletedDirs: number;
  freedAllocated: number;
  skipped: number;
  finished: boolean;
  // 用户取消了这次执行。
  cancelled: boolean;
}

export interface PlanHistory {
  plans: PlanHistoryEntry[];
  // 解析失败的日志行数，大于 0 说明历史不完整。
  skippedLines: number;
}

// 一次执行的历史详情。skipped 只保留前 200 条，skippedTotal 是实际总数。
export interface PlanDetail {
  planId: string;
  startedAt: number;
  freeBefore: number;
  freeAfter: number;
  finished: boolean;
  // 用户取消了这次执行。
  cancelled: boolean;
  rules: PlanDetailRule[];
  skipped: ExecSkip[];
  skippedTotal: number;
}

export interface PlanDetailRule {
  ruleId: string;
  deletedFiles: number;
  deletedDirs: number;
  freedAllocated: number;
  skipped: number;
}

// C 盘回收站当前内容。清空前要把这份数值回传给后端，对不上就不动。
export interface RecycleBinInfo {
  items: number;
  bytes: number;
}

// 可以作为隔离区的卷。
export interface QuarantineVolume {
  root: string;
  label: string;
  total: number;
  free: number;
}

// 隔离区设置卡：候选卷和当前选择。
export interface QuarantineVolumes {
  candidates: QuarantineVolume[];
  // 已经设置好的隔离根目录，null 表示还没选过。
  currentRoot: string | null;
  currentFree: number;
}

// 批次状态，与 Rust 端 BatchStage 的 key 一致。
export type QuarantineStage =
  | "planned"
  | "copying"
  | "copiedVerified"
  | "deletingSource"
  | "committed"
  | "restoring"
  | "restored"
  | "purging"
  | "purged"
  | "aborted"
  | "needsAttention";

// 隔离区页的一个批次。
export interface QuarantineBatch {
  batchId: string;
  // Unix 秒，界面按本地时区格式化。
  createdAt: number;
  ruleId: string;
  ruleName: string;
  stage: QuarantineStage;
  stageLabel: string;
  note: string;
  items: number;
  size: number;
  // 已经存放了多少天。
  ageDays: number;
  // 批次来自提权 helper，还原和彻底删除会再弹一次 UAC。
  requiresAdmin: boolean;
}

// 异常批次的人工处理方式。
export type QuarantineResolveAction = "continue" | "abandon";

export interface QuarantineRestoreResult {
  batchId: string;
  stage: QuarantineStage;
  stageLabel: string;
  restored: number;
  // 原位已有同名文件、因此没有还原的路径。
  conflicts: string[];
}

export interface QuarantineResolveResult {
  batchId: string;
  stage: QuarantineStage;
  stageLabel: string;
  // 源已删除、只留在隔离区的条目数。
  moved: number;
  // 源发生变化、源和副本都保留的条目数。
  kept: number;
}

// 提权 helper 执行一个内置操作的结果。
export interface ElevatedOpResult {
  // 操作的字符串形式，例如 leappstore-cicada。
  op: string;
  // done 表示完成，skipped 表示按规则跳过，failed 表示出错。
  status: "done" | "skipped" | "failed";
  detail: string;
  // 隔离批次的 ID，只有隔离类操作有。
  batchId: string | null;
  // 本次处理的条目数。
  items: number;
  // 还原时原位已有同名文件、因此没有还原的路径。
  conflicts: string[];
}

// 一次提权执行的完整结果。
export interface ElevatedResult {
  ops: ElevatedOpResult[];
}

// 系统维护操作的进度。DISM 清理会在运行中不断给出百分比。
export interface MaintenanceProgress {
  // 操作名，例如 dism-component-cleanup。
  op: string;
  // 0–100 的百分比。还没给出数字时为 null。
  percent: number | null;
  // 中文说明，直接显示。
  detail: string;
}

// 命令执行的进度。命令没有分项进度，只有开始和结束两个阶段。
export interface CommandProgress {
  stage: "running" | "done";
}

// 一次官方命令执行的结果。大小是命令执行前后的缓存占盘大小。
export interface CommandReport {
  ruleId: string;
  // 退出码。命令没跑起来时为 null。
  exitCode: number | null;
  // 等待超时。超时后命令仍在运行，进程没有被终止。
  timedOut: boolean;
  logicalBefore: number;
  allocatedBefore: number;
  logicalAfter: number;
  allocatedAfter: number;
  // 命令输出末尾，失败时用来看原因。
  output: string;
  // 命令已执行但日志写入失败时的提示。
  logError: string | null;
}

// 迁移工具名，与 Rust 端 ToolId 一致。
export type MigrationTool = "uv" | "pip" | "npm" | "nuget" | "gradle";

// 迁移方式：迁移旧缓存，或者允许工具在新位置重建。
export type MigrationMode = "migrate" | "rebuild";

// 一项缓存配置的当前状态。
export interface MigrationEntry {
  // 环境变量名，或者 `npm cache` 这样的配置项名。
  name: string;
  path: string;
  // 配置来源：环境变量 / 当前进程环境变量 / npmrc / 默认值。
  source: string;
  exists: boolean;
  size: number;
  onSystemDrive: boolean;
}

// 迁移页里一个工具的一行。
export interface MigrationToolState {
  tool: MigrationTool;
  label: string;
  entries: MigrationEntry[];
  totalSize: number;
  // 还有配置指向系统盘。
  onSystemDrive: boolean;
  // 迁移阶段，没迁移过时为 null。
  stage: string | null;
  stageLabel: string | null;
  note: string;
  // 新的缓存根目录。
  target: string | null;
  volumeRoot: string | null;
  mode: string | null;
  // 迁移前用过的目录里现在还存在、可以清理的那些。
  oldDirs: string[];
  // 相关进程里正在运行的那些，开始迁移前要提醒用户。
  processes: string[];
  installed: boolean;
  // 改配置或验证是否需要工具本身。Gradle 不需要。
  needsProgram: boolean;
  // 是否支持“允许重建”。Gradle 无法查询自己的缓存位置，只支持迁移。
  supportsRebuild: boolean;
  // 需要特别注意的提示，例如 Gradle 会整体搬走 .gradle。
  warning: string | null;
}

// 可以作为目标卷的卷。
export interface MigrationVolume {
  root: string;
  label: string;
  total: number;
  free: number;
}

export interface MigrationsState {
  tools: MigrationToolState[];
  volumes: MigrationVolume[];
  // 迁移记录主文件不可用、退回备份时的提示。
  storeNote: string | null;
  // 记录是从备份读出来的，确认使用备份之前后端拒绝写入。
  storeNeedsConfirm: boolean;
  // 启动恢复的结果和错误。
  recoveryNotes: string[];
}

// 一次迁移或撤销之后的结果。
export interface MigrationResult {
  tool: string;
  label: string;
  stage: string;
  stageLabel: string;
  note: string;
  target: string;
}

// 清理旧缓存目录的清单。planId 交给预览组件，条目路径仍然只在后端。
export interface MigrationCleanupPlan {
  planId: string;
  summary: PlanSummary;
}

// 目录迁移的阶段，与 Rust 端 DirStage 的 key 一致。
export type DirMigrationStage =
  | "precheck"
  | "copying"
  | "verified"
  | "sourceRenamed"
  | "junctionCreated"
  | "validated"
  | "awaitingUserTest"
  | "backupDeleted"
  | "done"
  | "returning"
  | "needsAttention"
  | "returned"
  | "failed";

// 一条目录迁移记录。
export interface DirMigration {
  id: string;
  source: string;
  target: string;
  backup: string;
  stage: DirMigrationStage;
  stageLabel: string;
  note: string;
  // Unix 秒，界面按本地时区格式化。
  updatedAt: number;
  // 目标目录还在。迁回之后它就是等着确认删除的旧副本。
  targetExists: boolean;
  // 源位置现在确实是一个 junction，才谈得上迁回。
  hasJunction: boolean;
  // 源目录已经被改到备份名下，可以“继续”。
  resumable: boolean;
  // 可以确认删除备份：备份还在，而且链接已经建好（或已经迁回）。
  canDeleteBackup: boolean;
}

export interface DirMigrationsState {
  records: DirMigration[];
  volumes: MigrationVolume[];
  // 迁移记录主文件不可用、退回备份时的提示。
  storeNote: string | null;
  // 记录是从备份读出来的，确认使用备份之前后端拒绝写入。
  storeNeedsConfirm: boolean;
  // 启动恢复的结果和错误。
  recoveryNotes: string[];
}

// 一个目录的资格检查结果。reasons 为空表示可以迁移。
export interface JunctionCheck {
  sourceId: number;
  path: string;
  allocated: number;
  reasons: string[];
  eligible: boolean;
}
