// CClear v2 全部 25 个 Tauri IPC 命令的类型化封装，与 docs/v2-ipc-contract.md 严格对应。
import { Channel, invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  AppSettings,
  AppSettingsPatch,
  CleanExecuteRequest,
  CleanHistoryDetail,
  CleanHistoryList,
  CleanProgress,
  CleanReport,
  DeepSlimAction,
  DeepSlimProgress,
  DeepSlimResult,
  DeepSlimState,
  DirNode,
  FileEntry,
  JunkScanProgress,
  JunkScanResult,
  ProcessCloseResult,
  RecycleActionResult,
  RulePackInfo,
  RunningProcessConflict,
  ScanProgress,
  ScanRoot,
  ScanSummary,
  SpaceTargetKind,
  VolumeInfo,
} from "./types";

// 1-2. 应用与卷信息
export function appInfo(): Promise<AppInfo> {
  return invoke<AppInfo>("app_info");
}

export function volumeInfo(): Promise<VolumeInfo[]> {
  return invoke<VolumeInfo[]>("volume_info");
}

// 3-9. 统一垃圾扫描与清理
export function junkScanStart(
  onProgress: (progress: JunkScanProgress) => void,
): Promise<JunkScanResult> {
  const channel = new Channel<JunkScanProgress>();
  channel.onmessage = onProgress;
  return invoke<JunkScanResult>("junk_scan_start", { onProgress: channel });
}

export function junkScanCancel(): Promise<void> {
  return invoke<void>("junk_scan_cancel");
}

export function cleanCheckProcesses(
  scanId: string,
  itemIds: string[],
): Promise<RunningProcessConflict[]> {
  return invoke<RunningProcessConflict[]>("clean_check_processes", { scanId, itemIds });
}

export function cleanCloseProcesses(processNames: string[]): Promise<ProcessCloseResult> {
  return invoke<ProcessCloseResult>("clean_close_processes", { processNames });
}

export function cleanExecute(
  request: CleanExecuteRequest,
  onProgress: (progress: CleanProgress) => void,
): Promise<CleanReport> {
  const channel = new Channel<CleanProgress>();
  channel.onmessage = onProgress;
  return invoke<CleanReport>("clean_execute", { request, onProgress: channel });
}

export function cleanCancel(): Promise<void> {
  return invoke<void>("clean_cancel");
}

export function openCleanItemLocation(
  scanId: string,
  itemId: string,
  sampleIndex: number | null,
): Promise<void> {
  return invoke<void>("open_clean_item_location", { scanId, itemId, sampleIndex });
}

// 10-11. 系统深度瘦身大户
export function deepSlimStatus(): Promise<DeepSlimState> {
  return invoke<DeepSlimState>("deep_slim_status");
}

export function deepSlimExecute(
  action: DeepSlimAction,
  onProgress: (progress: DeepSlimProgress) => void,
): Promise<DeepSlimResult> {
  const channel = new Channel<DeepSlimProgress>();
  channel.onmessage = onProgress;
  return invoke<DeepSlimResult>("deep_slim_execute", { action, onProgress: channel });
}

// 12-19. 空间分析
export function defaultScanRoots(): Promise<ScanRoot[]> {
  return invoke<ScanRoot[]>("default_scan_roots");
}

export function spaceScanStart(
  rootPath: string,
  useMft: boolean,
  onProgress: (progress: ScanProgress) => void,
): Promise<ScanSummary> {
  const channel = new Channel<ScanProgress>();
  channel.onmessage = onProgress;
  return invoke<ScanSummary>("space_scan_start", { rootPath, useMft, onProgress: channel });
}

export function spaceScanCancel(): Promise<void> {
  return invoke<void>("space_scan_cancel");
}

export function spaceScanSummary(): Promise<ScanSummary | null> {
  return invoke<ScanSummary | null>("space_scan_summary");
}

export function spaceScanChildren(nodeId: number | null): Promise<DirNode[]> {
  return invoke<DirNode[]>("space_scan_children", { nodeId });
}

export function spaceScanTopFiles(limit: number): Promise<FileEntry[]> {
  return invoke<FileEntry[]>("space_scan_top_files", { limit });
}

export function spaceOpenInExplorer(targetKind: SpaceTargetKind, id: number): Promise<void> {
  return invoke<void>("space_open_in_explorer", { targetKind, id });
}

export function spaceRecycleItem(
  targetKind: SpaceTargetKind,
  id: number,
): Promise<RecycleActionResult> {
  return invoke<RecycleActionResult>("space_recycle_item", { targetKind, id });
}

// 20-25. 历史、设置与规则包
export function historyList(days: number): Promise<CleanHistoryList> {
  return invoke<CleanHistoryList>("history_list", { days });
}

export function historyDetail(runId: string, days: number): Promise<CleanHistoryDetail> {
  return invoke<CleanHistoryDetail>("history_detail", { runId, days });
}

export function settingsGet(): Promise<AppSettings> {
  return invoke<AppSettings>("settings_get");
}

export function settingsSave(patch: AppSettingsPatch): Promise<AppSettings> {
  return invoke<AppSettings>("settings_save", { patch });
}

export function rulePackImport(jsonContent: string): Promise<RulePackInfo> {
  return invoke<RulePackInfo>("rule_pack_import", { jsonContent });
}

export function rulePackRemove(packId: string): Promise<void> {
  return invoke<void>("rule_pack_remove", { packId });
}
