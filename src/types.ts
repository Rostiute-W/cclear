// CClear v2 跨 IPC 类型定义，与 docs/v2-ipc-contract.md 和 Rust 端 camelCase 结构体严格一致。

export interface AppInfo {
  version: string;
  isAdmin: boolean;
  rulePackVersion: string;
  ruleCount: number;
}

export interface VolumeInfo {
  root: string;
  label: string;
  fs: string;
  total: number;
  free: number;
  isSystem: boolean;
}

export type CleanCategory = "system" | "browser" | "app" | "dev" | "leftover";

export type CleanSource =
  | "builtinRule"
  | "externalRule"
  | "heuristicElectron"
  | "heuristicUwp"
  | "heuristicOrphan"
  | "command"
  | "recycleBin"
  | "systemOp";

export type DeleteMode = "direct" | "recycleBin" | "command" | "systemOp";

export interface CleanItem {
  id: string;
  name: string;
  description: string;
  category: CleanCategory;
  source: CleanSource;
  /** 占盘大小（字节）；null 表示需管理员权限才能精确读取 */
  sizeBytes: number | null;
  fileCount: number;
  defaultSelected: boolean;
  requiresAdmin: boolean;
  deleteMode: DeleteMode;
  warning: string | null;
  /** 当前正在运行的相关进程名（提示用，不禁用勾选） */
  runningProcesses: string[];
  samplePaths: string[];
  anchors: string[];
  lastModifiedAt: number | null;
}

export interface CategoryGroup {
  category: CleanCategory;
  label: string;
  description: string;
  totalBytes: number;
  itemCount: number;
  items: CleanItem[];
}

export interface JunkScanProgress {
  phase: string;
  currentItem: string;
  foundBytes: number;
  foundItems: number;
  done: boolean;
  cancelled: boolean;
}

export interface JunkScanResult {
  scanId: string;
  elapsedMs: number;
  totalBytes: number;
  defaultSelectedBytes: number;
  categories: CategoryGroup[];
  deepSlim: DeepSlimState;
  cancelled: boolean;
}

export interface RunningProcessConflict {
  appName: string;
  processNames: string[];
  pids: number[];
  affectedItemIds: string[];
  affectedBytes: number;
}

export interface ProcessCloseResult {
  closed: string[];
  stillRunning: string[];
}

export interface CleanExecuteRequest {
  scanId: string;
  itemIds: string[];
  scheduleRebootDelete: boolean;
}

export interface CleanProgress {
  currentItemId: string;
  currentItemName: string;
  completedItems: number;
  totalItems: number;
  freedBytes: number;
  skippedFiles: number;
  stage: "user" | "command" | "recycle" | "elevated" | "done";
  done: boolean;
}

export interface CleanItemReport {
  itemId: string;
  name: string;
  category: CleanCategory;
  status: "done" | "partial" | "skipped" | "failed";
  freedBytes: number;
  deletedFiles: number;
  deletedDirs: number;
  recycledItems: number;
  skippedFiles: number;
  rebootScheduled: number;
  detail: string;
}

export interface CleanReport {
  runId: string;
  startedAt: number;
  elapsedMs: number;
  freedBytes: number;
  freeBefore: number;
  freeAfter: number;
  deletedFiles: number;
  deletedDirs: number;
  recycledItems: number;
  skippedFiles: number;
  rebootScheduled: number;
  items: CleanItemReport[];
  cancelled: boolean;
  error: string | null;
}

export type HibernateMode = "full" | "reduced" | "off" | "unknown";

export interface HibernateState {
  mode: HibernateMode;
  fileBytes: number | null;
}

export interface PagefileEntry {
  path: string;
  sizeBytes: number | null;
  onSystemDrive: boolean;
}

export interface WslVhdxInfo {
  name: string;
  path: string;
  sizeBytes: number;
  onSystemDrive: boolean;
}

export interface VssState {
  usedBytes: number | null;
  shadowCount: number | null;
}

export interface WinSxSState {
  reclaimableText: string | null;
  recommended: boolean | null;
  detail: string | null;
}

export interface DeepSlimState {
  hibernate: HibernateState;
  pagefiles: PagefileEntry[];
  wslVhdx: WslVhdxInfo[];
  vss: VssState;
  winsxs: WinSxSState;
}

export type DeepSlimAction =
  | "hibernateOff"
  | "hibernateReduced"
  | "hibernateOn"
  | "dismAnalyze"
  | "dismCleanup"
  | "vssCleanOldest"
  | "wslCompact";

export interface DeepSlimProgress {
  action: DeepSlimAction;
  percent: number | null;
  detail: string;
  done: boolean;
}

export interface DeepSlimResult {
  action: DeepSlimAction;
  status: "done" | "skipped" | "failed";
  detail: string;
  freeBefore: number;
  freeAfter: number;
  updatedState: DeepSlimState;
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
  mode: "standard" | "mft";
  done: boolean;
  cancelled: boolean;
  error: string | null;
}

export interface ScanSummary {
  root: string;
  mode: "standard" | "mft";
  logical: number;
  allocated: number;
  files: number;
  dirs: number;
  deniedDirs: number;
  elapsedMs: number;
  cancelled: boolean;
  fallbackNote: string | null;
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

export type SpaceTargetKind = "node" | "topFile";

export interface RecycleActionResult {
  path: string;
  freedBytes: number;
  detail: string;
}

export interface CleanHistoryEntry {
  runId: string;
  startedAt: number;
  elapsedMs: number;
  itemNames: string[];
  freedBytes: number;
  freeBefore: number;
  freeAfter: number;
  deletedFiles: number;
  recycledItems: number;
  skippedFiles: number;
  rebootScheduled: number;
  cancelled: boolean;
}

export interface CleanHistoryList {
  runs: CleanHistoryEntry[];
  skippedLines: number;
}

export interface CleanHistoryDetail {
  runId: string;
  startedAt: number;
  elapsedMs: number;
  freedBytes: number;
  freeBefore: number;
  freeAfter: number;
  cancelled: boolean;
  items: CleanItemReport[];
}

export interface RulePackRuleSummary {
  id: string;
  name: string;
  deleteMode: DeleteMode;
  targets: string[];
}

export interface RulePackInfo {
  id: string;
  name: string;
  version: string;
  description: string;
  ruleCount: number;
  builtin: boolean;
  rulesSummary: RulePackRuleSummary[];
}

export interface AppSettings {
  whitelistPaths: string[];
  scheduleRebootDeleteDefault: boolean;
  minTempAgeHours: number;
  rulePacks: RulePackInfo[];
}

export interface AppSettingsPatch {
  whitelistPaths?: string[];
  scheduleRebootDeleteDefault?: boolean;
  minTempAgeHours?: number;
}
