// 所有 invoke 的类型化封装。扫描路径只能由这里按用户选择发起，界面不拼路径。
import { Channel, invoke } from "@tauri-apps/api/core";
import type {
  AiAnswer,
  AiSettings,
  AppInfo,
  CandidateInfo,
  CommandProgress,
  CommandReport,
  DirMigration,
  DirMigrationsState,
  DirNode,
  ElevatedResult,
  EnvState,
  ExecProgress,
  ExecReport,
  FileEntry,
  JunctionCheck,
  LockReport,
  MaintenanceProgress,
  MigrationCleanupPlan,
  MigrationMode,
  MigrationResult,
  MigrationsState,
  MigrationTool,
  MigrationToolState,
  PlanDetail,
  PlanHistory,
  PlanItem,
  PlanSummary,
  QuarantineBatch,
  QuarantineResolveAction,
  QuarantineResolveResult,
  QuarantineRestoreResult,
  QuarantineVolumes,
  RecycleBinInfo,
  RuleInfo,
  ScanProgress,
  ScanRoot,
  ScanSummary,
  VolumeInfo,
} from "./types";

export function appInfo(): Promise<AppInfo> {
  return invoke<AppInfo>("app_info");
}

export function envState(): Promise<EnvState> {
  return invoke<EnvState>("env_state");
}

// AI 咨询：设置里只回传有没有 Key；提问只传规则 ID 和可选问题，规则信息由后端重新评估。
export function aiSettingsGet(): Promise<AiSettings> {
  return invoke<AiSettings>("ai_settings_get");
}

// apiKey 为 null 保留已保存的 Key，空串清除，其他值由后端加密保存。
export function aiSettingsSave(
  baseUrl: string,
  model: string,
  apiKey: string | null,
): Promise<AiSettings> {
  return invoke<AiSettings>("ai_settings_save", { baseUrl, model, apiKey });
}

export function aiAskRule(ruleId: string, question: string | null): Promise<AiAnswer> {
  return invoke<AiAnswer>("ai_ask_rule", { ruleId, question });
}

export function rulesCatalog(): Promise<RuleInfo[]> {
  return invoke<RuleInfo[]>("rules_catalog");
}

export function rulesEvaluate(): Promise<CandidateInfo[]> {
  return invoke<CandidateInfo[]>("rules_evaluate");
}

// 只传规则 ID：路径由后端按规则重新解析。
export function candidateLocks(ruleId: string): Promise<LockReport> {
  return invoke<LockReport>("candidate_locks", { ruleId });
}

export function openCandidateInExplorer(
  ruleId: string,
  sampleIndex: number | null,
): Promise<void> {
  return invoke<void>("open_candidate_in_explorer", { ruleId, sampleIndex });
}

// 命令类规则只传规则 ID 和是否已二次确认：命令和缓存路径都由后端命令表决定。
export function commandExecute(
  ruleId: string,
  secondConfirm: boolean,
  onProgress: (progress: CommandProgress) => void,
): Promise<CommandReport> {
  const channel = new Channel<CommandProgress>();
  channel.onmessage = (progress) => {
    onProgress(progress);
  };

  return invoke<CommandReport>("command_execute", {
    ruleId,
    secondConfirm,
    onProgress: channel,
  });
}

// 需要管理员权限的清理项：只传规则 ID 列表，内置操作和安全路径都由后端决定。
// 多条规则合成一次提权调用，只弹一次授权。
export function elevatedExecute(ruleIds: string[]): Promise<ElevatedResult> {
  return invoke<ElevatedResult>("elevated_execute", { ruleIds });
}

// 分析组件存储（DISM，只读）。会弹一次授权提示。
export function maintenanceAnalyze(): Promise<ElevatedResult> {
  return invoke<ElevatedResult>("maintenance_analyze");
}

// 系统维护：只传规则 ID，选中的项合成一次提权调用，只弹一次授权。
export function maintenanceExecute(
  ruleIds: string[],
  onProgress: (progress: MaintenanceProgress) => void,
): Promise<ElevatedResult> {
  const channel = new Channel<MaintenanceProgress>();
  channel.onmessage = (progress) => {
    onProgress(progress);
  };

  return invoke<ElevatedResult>("maintenance_execute", { ruleIds, onProgress: channel });
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

// 冻结清单：只传规则 ID，清单内容由后端持有，界面拿不到执行路径。
export function planBuild(ruleIds: string[]): Promise<PlanSummary> {
  return invoke<PlanSummary>("plan_build", { ruleIds });
}

export function planItems(
  planId: string,
  ruleId: string,
  offset: number,
  limit: number,
): Promise<PlanItem[]> {
  return invoke<PlanItem[]>("plan_items", { planId, ruleId, offset, limit });
}

export function planCancel(): Promise<void> {
  return invoke<void>("plan_cancel");
}

export function planExecute(
  planId: string,
  ruleIds: string[],
  onProgress: (progress: ExecProgress) => void,
): Promise<ExecReport> {
  const channel = new Channel<ExecProgress>();
  channel.onmessage = (progress) => {
    onProgress(progress);
  };

  return invoke<ExecReport>("plan_execute", { planId, ruleIds, onProgress: channel });
}

export function historyList(days: number): Promise<PlanHistory> {
  return invoke<PlanHistory>("history_list", { days });
}

// 回收站：只传上次查询到的条目数和大小，路径固定是 C 盘，界面不拼路径。
export function recycleBinQuery(): Promise<RecycleBinInfo> {
  return invoke<RecycleBinInfo>("recycle_bin_query");
}

export function recycleBinEmpty(items: number, bytes: number): Promise<RecycleBinInfo> {
  return invoke<RecycleBinInfo>("recycle_bin_empty", { expectedItems: items, expectedBytes: bytes });
}

// 历史详情只传计划 ID：条目和跳过原因都从日志里读，界面不拼路径。
export function historyDetail(planId: string, days: number): Promise<PlanDetail> {
  return invoke<PlanDetail>("history_detail", { planId, days });
}

// 隔离区：只传卷根和批次 ID，条目路径全部来自后端的批次清单。
export function quarantineVolumes(): Promise<QuarantineVolumes> {
  return invoke<QuarantineVolumes>("quarantine_volumes");
}

export function quarantineSetVolume(volumeRoot: string): Promise<QuarantineVolumes> {
  return invoke<QuarantineVolumes>("quarantine_set_volume", { volumeRoot });
}

export function quarantineBatches(): Promise<QuarantineBatch[]> {
  return invoke<QuarantineBatch[]>("quarantine_batches");
}

export function quarantineRestore(batchId: string): Promise<QuarantineRestoreResult> {
  return invoke<QuarantineRestoreResult>("quarantine_restore", { batchId });
}

export function quarantinePurge(batchId: string): Promise<void> {
  return invoke<void>("quarantine_purge", { batchId });
}

export function quarantineResolve(
  batchId: string,
  action: QuarantineResolveAction,
): Promise<QuarantineResolveResult> {
  return invoke<QuarantineResolveResult>("quarantine_resolve", { batchId, action });
}

export function quarantineOverdue(): Promise<QuarantineBatch[]> {
  return invoke<QuarantineBatch[]>("quarantine_overdue");
}

// 迁移：只传工具名、模式名和目标卷根。缓存在哪、配置怎么写、用什么命令验证都由后端决定。
export function migrationsList(): Promise<MigrationsState> {
  return invoke<MigrationsState>("migrations_list");
}

export function migrationInspect(tool: MigrationTool): Promise<MigrationToolState> {
  return invoke<MigrationToolState>("migration_inspect", { tool });
}

export function migrationStart(
  tool: MigrationTool,
  volumeRoot: string,
  mode: MigrationMode,
): Promise<MigrationResult> {
  return invoke<MigrationResult>("migration_start", { tool, volumeRoot, mode });
}

export function migrationCleanupOld(tool: MigrationTool): Promise<MigrationCleanupPlan> {
  return invoke<MigrationCleanupPlan>("migration_cleanup_old", { tool });
}

export function migrationRevert(tool: MigrationTool): Promise<MigrationResult> {
  return invoke<MigrationResult>("migration_revert", { tool });
}

// 确认使用备份里的迁移记录：确认之后后端才允许把它写回主文件。
export function migrationConfirmBackup(): Promise<void> {
  return invoke<void>("migration_confirm_backup");
}

// 目录迁移：只传扫描节点 ID 和目标卷根，源路径从上次扫描结果里取。
export function junctionState(): Promise<DirMigrationsState> {
  return invoke<DirMigrationsState>("junction_state");
}

export function junctionCheck(sourceId: number): Promise<JunctionCheck> {
  return invoke<JunctionCheck>("junction_check", { sourceId });
}

export function junctionStart(sourceId: number, volumeRoot: string): Promise<DirMigration> {
  return invoke<DirMigration>("junction_start", { sourceId, volumeRoot });
}

export function junctionContinue(id: string): Promise<DirMigration> {
  return invoke<DirMigration>("junction_continue", { id });
}

export function junctionReturn(id: string): Promise<DirMigration> {
  return invoke<DirMigration>("junction_return", { id });
}

export function junctionDeleteBackup(id: string): Promise<DirMigration> {
  return invoke<DirMigration>("junction_delete_backup", { id });
}

export function junctionDiscardTarget(id: string): Promise<DirMigration> {
  return invoke<DirMigration>("junction_discard_target", { id });
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
