// 所有 invoke 的类型化封装。扫描路径只能由这里按用户选择发起，界面不拼路径。
import { Channel, invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  DirNode,
  FileEntry,
  ScanProgress,
  ScanRoot,
  ScanSummary,
  VolumeInfo,
} from "./types";

export function appInfo(): Promise<AppInfo> {
  return invoke<AppInfo>("app_info");
}

export function volumeInfo(): Promise<VolumeInfo[]> {
  return invoke<VolumeInfo[]>("volume_info");
}

export function defaultScanRoots(): Promise<ScanRoot[]> {
  return invoke<ScanRoot[]>("default_scan_roots");
}

export function scanCancel(): Promise<void> {
  return invoke<void>("scan_cancel");
}

export function scanSummary(): Promise<ScanSummary | null> {
  return invoke<ScanSummary | null>("scan_summary");
}

export function scanChildren(nodeId: number | null): Promise<DirNode[]> {
  return invoke<DirNode[]>("scan_children", { nodeId });
}

export function scanTopFiles(limit: number): Promise<FileEntry[]> {
  return invoke<FileEntry[]>("scan_top_files", { limit });
}

export function openNodeInExplorer(nodeId: number): Promise<void> {
  return invoke<void>("open_node_in_explorer", { nodeId });
}

export function openTopFileInExplorer(index: number): Promise<void> {
  return invoke<void>("open_top_file_in_explorer", { index });
}

type ScanListener = (progress: ScanProgress) => void;

const scanListeners = new Set<ScanListener>();
let latestProgress: ScanProgress | null = null;

// 扫描进度放在模块作用域：切换页面会卸载组件，但后台扫描还在继续，
// 回到总览页时靠这里恢复进度，而不是显示成“尚未扫描”。
export function subscribeScan(listener: ScanListener): () => void {
  scanListeners.add(listener);
  return () => {
    scanListeners.delete(listener);
  };
}

export function currentScanProgress(): ScanProgress | null {
  return latestProgress;
}

export async function scanStart(root: string): Promise<string> {
  const channel = new Channel<ScanProgress>();
  channel.onmessage = (progress) => {
    latestProgress = progress;
    for (const listener of scanListeners) {
      listener(progress);
    }
  };

  return invoke<string>("scan_start", { root, onProgress: channel });
}
