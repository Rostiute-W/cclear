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
