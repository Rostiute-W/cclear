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
export interface LockReport {
  checked: number;
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
