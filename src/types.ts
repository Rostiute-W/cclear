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
