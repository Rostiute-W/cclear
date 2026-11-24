# CClear v2 前后端冻结 IPC 契约

> 本契约是 **Codex（Rust 后端）** 与 **Antigravity（React 前端）** 并行开发的唯一接口标准。  
> 所有跨 IPC 结构体均使用 `#[serde(rename_all = "camelCase")]`，命令返回 `Result<T, String>`（错误信息为中文可读文案）。  
> **规则**：所有命令必须在 `src-tauri/src/commands/`、`src-tauri/src/lib.rs`、`src-tauri/build.rs`、`src-tauri/capabilities/main.json`、`src/api.ts` 保持严格一致。

---

## 1. Tauri 命令清单（共 25 个）

### `src-tauri/build.rs` 的 `AppManifest::commands` 精确列表
```rust
&[
    "app_info",
    "volume_info",
    "junk_scan_start",
    "junk_scan_cancel",
    "clean_check_processes",
    "clean_close_processes",
    "clean_execute",
    "clean_cancel",
    "open_clean_item_location",
    "deep_slim_status",
    "deep_slim_execute",
    "default_scan_roots",
    "space_scan_start",
    "space_scan_cancel",
    "space_scan_summary",
    "space_scan_children",
    "space_scan_top_files",
    "space_open_in_explorer",
    "space_recycle_item",
    "history_list",
    "history_detail",
    "settings_get",
    "settings_save",
    "rule_pack_import",
    "rule_pack_remove",
]
```

### `src-tauri/capabilities/main.json` 的 `permissions` 精确列表
每个命令对应 `"allow-<kebab-case>"`，例如 `"allow-app-info"`、`"allow-junk-scan-start"` 等，共 25 条，**禁止** `core:default`、`remote` 及任何危险插件权限。

---

## 2. TypeScript / Rust 类型定义（一一对应）

### 2.1 应用与卷信息
```ts
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
```

### 2.2 统一垃圾扫描与清理（首页）
```ts
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
  /** 最多 10 条代表性样例路径 */
  samplePaths: string[];
  anchors: string[];
  /** 最后修改时间（Unix 秒，卸载残留项用于辅助判断，其他项可为 null） */
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
```

### 2.3 系统深度瘦身大户
```ts
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
  /** 已用卷影空间（字节），普通权限读不到时为 null */
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
```

### 2.4 空间分析（第 2 页）
```ts
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
```

### 2.5 历史与设置（第 3 页）
```ts
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
```

---

## 3. 每个命令的精确参数签名（Tauri `invoke` 参数名）

| 命令名 | 参数（camelCase JSON key） | 返回值 |
|---|---|---|
| `app_info` | 无 | `AppInfo` |
| `volume_info` | 无 | `VolumeInfo[]` |
| `junk_scan_start` | `{ onProgress: Channel<JunkScanProgress> }` | `JunkScanResult` |
| `junk_scan_cancel` | 无 | `void` |
| `clean_check_processes` | `{ scanId: string, itemIds: string[] }` | `RunningProcessConflict[]` |
| `clean_close_processes` | `{ processNames: string[] }` | `ProcessCloseResult` |
| `clean_execute` | `{ request: CleanExecuteRequest, onProgress: Channel<CleanProgress> }` | `CleanReport` |
| `clean_cancel` | 无 | `void` |
| `open_clean_item_location` | `{ scanId: string, itemId: string, sampleIndex: number | null }` | `void` |
| `deep_slim_status` | 无 | `DeepSlimState` |
| `deep_slim_execute` | `{ action: DeepSlimAction, onProgress: Channel<DeepSlimProgress> }` | `DeepSlimResult` |
| `default_scan_roots` | 无 | `ScanRoot[]` |
| `space_scan_start` | `{ rootPath: string, useMft: boolean, onProgress: Channel<ScanProgress> }` | `ScanSummary` |
| `space_scan_cancel` | 无 | `void` |
| `space_scan_summary` | 无 | `ScanSummary | null` |
| `space_scan_children` | `{ nodeId: number | null }` | `DirNode[]` |
| `space_scan_top_files` | `{ limit: number }` | `FileEntry[]` |
| `space_open_in_explorer` | `{ targetKind: SpaceTargetKind, id: number }` | `void` |
| `space_recycle_item` | `{ targetKind: SpaceTargetKind, id: number }` | `RecycleActionResult` |
| `history_list` | `{ days: number }` | `CleanHistoryList` |
| `history_detail` | `{ runId: string, days: number }` | `CleanHistoryDetail` |
| `settings_get` | 无 | `AppSettings` |
| `settings_save` | `{ patch: AppSettingsPatch }` | `AppSettings` |
| `rule_pack_import` | `{ jsonContent: string }` | `RulePackInfo` |
| `rule_pack_remove` | `{ packId: string }` | `void` |

> **注意**：Rust 端 Tauri 命令函数签名使用 `snake_case` 参数名（Tauri 2 默认自动将 JS 的 `camelCase` 映射到 Rust 的 `snake_case`），例如 `scan_id: String` 对应 JS `{ scanId }`、`use_mft: bool` 对应 `{ useMft }`、`on_progress: Channel<...>` 对应 `{ onProgress }`。
