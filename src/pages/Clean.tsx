import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  cleanCancel,
  cleanCheckProcesses,
  cleanCloseProcesses,
  cleanExecute,
  deepSlimExecute,
  deepSlimStatus,
  junkScanCancel,
  junkScanStart,
  openCleanItemLocation,
  settingsGet,
  volumeInfo,
} from "../api";
import {
  IconCheck,
  IconChevron,
  IconClean,
  IconCpu,
  IconFolder,
  IconGlobe,
  IconGrid,
  IconLock,
  IconRefresh,
  IconShield,
  IconTerminal,
  IconTrash,
  IconWarning,
  IconZap,
} from "../components/Icons";
import { AlertBanner, StatusBadge } from "../components/Ui";
import { formatDuration, formatSize, formatTime } from "../format";
import type {
  CategoryGroup,
  CleanCategory,
  CleanItem,
  CleanProgress,
  CleanReport,
  DeepSlimAction,
  DeepSlimProgress,
  DeepSlimResult,
  DeepSlimState,
  JunkScanProgress,
  JunkScanResult,
  RunningProcessConflict,
  VolumeInfo,
} from "../types";

const CATEGORY_ICON: Record<CleanCategory, React.ReactNode> = {
  system: <IconCpu width={16} height={16} />,
  browser: <IconGlobe width={16} height={16} />,
  app: <IconGrid width={16} height={16} />,
  dev: <IconTerminal width={16} height={16} />,
  leftover: <IconTrash width={16} height={16} />,
};

function sourceTag(item: CleanItem): { label: string; tone: "info" | "quarantine" | "neutral" } | null {
  switch (item.source) {
    case "heuristicElectron":
      return { label: "自动发现 · Electron", tone: "info" };
    case "heuristicUwp":
      return { label: "自动发现 · 商店应用", tone: "info" };
    case "heuristicOrphan":
      return { label: "卸载残留 · 移入回收站", tone: "quarantine" };
    case "externalRule":
      return { label: "扩展规则", tone: "neutral" };
    case "recycleBin":
      return null;
    default:
      return item.deleteMode === "recycleBin"
        ? { label: "移入回收站", tone: "quarantine" }
        : null;
  }
}

function hibernateModeLabel(mode: DeepSlimState["hibernate"]["mode"]): string {
  switch (mode) {
    case "full":
      return "完整开启";
    case "reduced":
      return "精简模式（仅快速启动）";
    case "off":
      return "已关闭";
    default:
      return "未知";
  }
}

function IndeterminateCheckbox({
  checked,
  indeterminate,
  disabled,
  ariaLabel,
  onChange,
}: {
  checked: boolean;
  indeterminate: boolean;
  disabled?: boolean;
  ariaLabel: string;
  onChange: () => void;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) {
      ref.current.indeterminate = indeterminate;
    }
  }, [indeterminate]);

  return (
    <input
      ref={ref}
      type="checkbox"
      className="checkbox"
      checked={checked}
      disabled={disabled}
      aria-label={ariaLabel}
      onChange={onChange}
      onClick={(event) => event.stopPropagation()}
    />
  );
}

export function CleanPage({
  onCleaned,
  onOpenSpace,
}: {
  onCleaned: () => void;
  onOpenSpace: () => void;
}) {
  const [volumes, setVolumes] = useState<VolumeInfo[]>([]);
  const [deepSlim, setDeepSlim] = useState<DeepSlimState | null>(null);
  const [scheduleRebootDelete, setScheduleRebootDelete] = useState(false);

  const [scanning, setScanning] = useState(false);
  const [scanProgress, setScanProgress] = useState<JunkScanProgress | null>(null);
  const [scanResult, setScanResult] = useState<JunkScanResult | null>(null);

  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [openCategories, setOpenCategories] = useState<Set<CleanCategory>>(
    new Set<CleanCategory>(["system", "browser", "app", "dev"]),
  );
  const [expandedItems, setExpandedItems] = useState<Set<string>>(new Set());

  const [cleaning, setCleaning] = useState(false);
  const [cleanProgress, setCleanProgress] = useState<CleanProgress | null>(null);
  const [cleanReport, setCleanReport] = useState<CleanReport | null>(null);
  const [reportExpanded, setReportExpanded] = useState(false);

  // 进程冲突弹窗状态
  const [conflicts, setConflicts] = useState<RunningProcessConflict[] | null>(null);
  const [closingProcs, setClosingProcs] = useState(false);
  const [recheckingProcs, setRecheckingProcs] = useState(false);
  const [closeNotice, setCloseNotice] = useState<string | null>(null);

  // 深度瘦身执行状态
  const [slimBusy, setSlimBusy] = useState<DeepSlimAction | null>(null);
  const [slimProgress, setSlimProgress] = useState<DeepSlimProgress | null>(null);
  const [slimResult, setSlimResult] = useState<DeepSlimResult | null>(null);

  const [error, setError] = useState<string | null>(null);

  const refreshBase = useCallback(() => {
    volumeInfo()
      .then(setVolumes)
      .catch(() => {});
    deepSlimStatus()
      .then(setDeepSlim)
      .catch(() => {});
  }, []);

  useEffect(() => {
    refreshBase();
    settingsGet()
      .then((settings) => setScheduleRebootDelete(settings.scheduleRebootDeleteDefault))
      .catch(() => {});
  }, [refreshBase]);

  const systemVol = useMemo(
    () => volumes.find((vol) => vol.isSystem) ?? volumes[0] ?? null,
    [volumes],
  );

  const usedBytes = systemVol ? Math.max(0, systemVol.total - systemVol.free) : 0;
  const usedRatio = systemVol && systemVol.total > 0 ? usedBytes / systemVol.total : 0;
  const gaugeTone =
    usedRatio >= 0.85 ? "crit" : usedRatio >= 0.7 ? "warn" : "ok";

  const allItems = useMemo(
    () => (scanResult ? scanResult.categories.flatMap((group) => group.items) : []),
    [scanResult],
  );

  const selectedSummary = useMemo(() => {
    let bytes = 0;
    let count = 0;
    let adminCount = 0;
    let recycleCount = 0;
    for (const item of allItems) {
      if (selected.has(item.id)) {
        count += 1;
        bytes += item.sizeBytes ?? 0;
        if (item.requiresAdmin) adminCount += 1;
        if (item.deleteMode === "recycleBin" && item.source !== "recycleBin") recycleCount += 1;
      }
    }
    return { bytes, count, adminCount, recycleCount };
  }, [allItems, selected]);

  const busy = scanning || cleaning || slimBusy !== null;

  const startScan = () => {
    if (busy) return;
    setError(null);
    setCleanReport(null);
    setConflicts(null);
    setCloseNotice(null);
    setScanning(true);
    setScanProgress({
      phase: "准备扫描",
      currentItem: "正在加载规则库与探测器…",
      foundBytes: 0,
      foundItems: 0,
      done: false,
      cancelled: false,
    });

    junkScanStart((progress) => setScanProgress(progress))
      .then((result) => {
        setScanResult(result);
        setDeepSlim(result.deepSlim);
        const defaults = new Set<string>();
        const nonEmptyCats = new Set<CleanCategory>();
        for (const group of result.categories) {
          if (group.items.length > 0 && group.category !== "leftover") {
            nonEmptyCats.add(group.category);
          }
          for (const item of group.items) {
            if (item.defaultSelected) {
              defaults.add(item.id);
            }
          }
        }
        setSelected(defaults);
        setOpenCategories(nonEmptyCats);
        refreshBase();
      })
      .catch((reason) => setError(String(reason)))
      .finally(() => {
        setScanning(false);
        setScanProgress(null);
      });
  };

  const toggleItem = (id: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const toggleCategorySelect = (group: CategoryGroup) => {
    setSelected((prev) => {
      const next = new Set(prev);
      const allChecked = group.items.every((item) => next.has(item.id));
      for (const item of group.items) {
        if (allChecked) next.delete(item.id);
        else next.add(item.id);
      }
      return next;
    });
  };

  const toggleCategoryOpen = (category: CleanCategory) => {
    setOpenCategories((prev) => {
      const next = new Set(prev);
      if (next.has(category)) next.delete(category);
      else next.add(category);
      return next;
    });
  };

  const toggleItemExpand = (id: string) => {
    setExpandedItems((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const applyPreset = (mode: "recommended" | "all" | "none") => {
    if (!scanResult) return;
    if (mode === "none") {
      setSelected(new Set());
      return;
    }
    const next = new Set<string>();
    for (const item of allItems) {
      if (mode === "all" || item.defaultSelected) {
        next.add(item.id);
      }
    }
    setSelected(next);
  };

  const runCleanNow = (itemIds: string[]) => {
    if (!scanResult || itemIds.length === 0) return;
    setConflicts(null);
    setError(null);
    setCleaning(true);
    setCleanReport(null);
    setReportExpanded(false);
    setCleanProgress({
      currentItemId: "",
      currentItemName: "正在准备清理…",
      completedItems: 0,
      totalItems: itemIds.length,
      freedBytes: 0,
      skippedFiles: 0,
      stage: "user",
      done: false,
    });

    cleanExecute(
      {
        scanId: scanResult.scanId,
        itemIds,
        scheduleRebootDelete,
      },
      (progress) => setCleanProgress(progress),
    )
      .then((report) => {
        setCleanReport(report);
        // 清理完成后后端已释放本次扫描快照，前端同步清空陈旧列表避免误点失效操作
        setScanResult(null);
        setSelected(new Set());
        setExpandedItems(new Set());
        refreshBase();
        onCleaned();
      })
      .catch((reason) => setError(String(reason)))
      .finally(() => {
        setCleaning(false);
        setCleanProgress(null);
      });
  };

  const handleCleanClick = async () => {
    if (!scanResult || selectedSummary.count === 0 || busy) return;
    setError(null);
    setCloseNotice(null);
    const ids = Array.from(selected);

    try {
      const running = await cleanCheckProcesses(scanResult.scanId, ids);
      if (running.length > 0) {
        setConflicts(running);
        return;
      }
    } catch (reason) {
      setError(String(reason));
      return;
    }

    runCleanNow(ids);
  };

  useEffect(() => {
    if (!conflicts) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !closingProcs && !recheckingProcs) {
        setConflicts(null);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [conflicts, closingProcs, recheckingProcs]);

  const handleRecheckProcesses = async () => {
    if (!scanResult) return;
    setRecheckingProcs(true);
    setError(null);
    const ids = Array.from(selected);
    try {
      const stillRunning = await cleanCheckProcesses(scanResult.scanId, ids);
      if (stillRunning.length === 0) {
        setConflicts(null);
        runCleanNow(ids);
      } else {
        setConflicts(stillRunning);
      }
    } catch (reason) {
      setError(String(reason));
    } finally {
      setRecheckingProcs(false);
    }
  };

  const handleGracefulCloseAndClean = async () => {
    if (!conflicts || !scanResult) return;
    setClosingProcs(true);
    setCloseNotice(null);
    const names = Array.from(new Set(conflicts.flatMap((c) => c.processNames)));
    try {
      const result = await cleanCloseProcesses(names);
      if (result.stillRunning.length > 0) {
        setCloseNotice(
          `以下程序未能自动退出（${result.stillRunning.join("、")}），将自动跳过其被占用的文件并清理其余部分。`,
        );
      }
      runCleanNow(Array.from(selected));
    } catch (reason) {
      setError(String(reason));
    } finally {
      setClosingProcs(false);
    }
  };

  const runDeepSlim = (action: DeepSlimAction, confirmMessage?: string) => {
    if (busy) return;
    if (confirmMessage && !window.confirm(confirmMessage)) {
      return;
    }
    setError(null);
    setSlimResult(null);
    setSlimBusy(action);
    setSlimProgress({
      action,
      percent: null,
      detail: "等待管理员授权并启动操作…",
      done: false,
    });

    deepSlimExecute(action, (progress) => setSlimProgress(progress))
      .then((result) => {
        setSlimResult(result);
        setDeepSlim(result.updatedState);
        refreshBase();
        onCleaned();
      })
      .catch((reason) => setError(String(reason)))
      .finally(() => {
        setSlimBusy(null);
        setSlimProgress(null);
      });
  };

  const actualFreed = cleanReport
    ? Math.max(0, cleanReport.freeAfter - cleanReport.freeBefore)
    : 0;

  return (
    <section className="page-section">
      {/* 顶部 C 盘空间英雄卡 */}
      <div className={`hero-card hero-${gaugeTone}`}>
        <div className="hero-top">
          <div className="hero-drive">
            <div className="hero-title-row">
              <h2 className="hero-title">
                {systemVol ? `系统盘 (${systemVol.root.replace(/\\$/, "")})` : "系统盘 (C:)"}
              </h2>
              {systemVol && (
                <StatusBadge
                  tone={
                    gaugeTone === "crit" ? "danger" : gaugeTone === "warn" ? "caution" : "safe"
                  }
                >
                  {gaugeTone === "crit"
                    ? "空间紧张"
                    : gaugeTone === "warn"
                      ? "偏满"
                      : "空间充足"}
                </StatusBadge>
              )}
            </div>
            {systemVol ? (
              <p className="hero-metrics td-num">
                剩余 <strong>{formatSize(systemVol.free)}</strong> / 共{" "}
                {formatSize(systemVol.total)}（已用 {(usedRatio * 100).toFixed(1)}%）
              </p>
            ) : (
              <p className="hero-metrics muted">正在读取 C 盘容量…</p>
            )}
            <div className="gauge-track" role="progressbar" aria-valuenow={Math.round(usedRatio * 100)} aria-valuemin={0} aria-valuemax={100}>
              <div
                className={`gauge-fill gauge-${gaugeTone}`}
                style={{ width: `${Math.min(100, usedRatio * 100)}%` }}
              />
            </div>
          </div>

          <div className="hero-cta">
            {!scanning && !cleaning && !scanResult && (
              <button
                type="button"
                className="btn btn-primary btn-hero"
                disabled={busy}
                onClick={startScan}
              >
                <IconClean width={18} height={18} />
                <span>一键扫描垃圾</span>
              </button>
            )}

            {scanning && (
              <button
                type="button"
                className="btn btn-subtle btn-hero"
                onClick={() => junkScanCancel().catch(() => {})}
              >
                <span>取消扫描</span>
              </button>
            )}

            {!scanning && !cleaning && scanResult && (
              <div className="hero-cta-group">
                <button
                  type="button"
                  className="btn btn-primary btn-hero"
                  disabled={selectedSummary.count === 0 || busy}
                  onClick={handleCleanClick}
                >
                  <IconZap width={18} height={18} />
                  <span>
                    {selectedSummary.count > 0
                      ? `立即清理 · ${formatSize(selectedSummary.bytes)}`
                      : "请先勾选清理项"}
                  </span>
                </button>
                <button
                  type="button"
                  className="btn btn-subtle"
                  disabled={busy}
                  onClick={startScan}
                >
                  <IconRefresh width={14} height={14} />
                  <span>重新扫描</span>
                </button>
              </div>
            )}

            {cleaning && (
              <button
                type="button"
                className="btn btn-danger btn-hero"
                onClick={() => cleanCancel().catch(() => {})}
              >
                <span>停止清理</span>
              </button>
            )}
          </div>
        </div>

        {/* 扫描中实时进度 */}
        {scanning && scanProgress && (
          <div className="live-progress">
            <div className="live-progress-row">
              <span className="live-phase">{scanProgress.phase}</span>
              <span className="muted truncate">{scanProgress.currentItem}</span>
              <span className="td-num live-bytes">
                已发现 {formatSize(scanProgress.foundBytes)}（{scanProgress.foundItems} 项）
              </span>
            </div>
            <div className="bar-indeterminate">
              <div className="bar-indeterminate-inner" />
            </div>
          </div>
        )}

        {/* 清理中实时进度 */}
        {cleaning && cleanProgress && (
          <div className="live-progress">
            <div className="live-progress-row">
              <span className="live-phase">
                正在清理 ({cleanProgress.completedItems}/{cleanProgress.totalItems})
              </span>
              <span className="muted truncate">{cleanProgress.currentItemName}</span>
              <span className="td-num live-bytes">
                已释放 {formatSize(cleanProgress.freedBytes)}
              </span>
            </div>
            <div className="bar">
              <div
                className="bar-fill"
                style={{
                  width: `${
                    cleanProgress.totalItems > 0
                      ? Math.min(100, (cleanProgress.completedItems / cleanProgress.totalItems) * 100)
                      : 0
                  }%`,
                }}
              />
            </div>
          </div>
        )}

        {/* 扫描完成摘要工具栏 */}
        {!scanning && !cleaning && scanResult && (
          <div className="hero-toolbar">
            <div className="hero-toolbar-left td-num">
              <span>
                扫描耗时 {formatDuration(scanResult.elapsedMs)} · 共发现{" "}
                <strong>{formatSize(scanResult.totalBytes)}</strong> · 已选{" "}
                <strong>{selectedSummary.count}</strong> 项（
                <strong>{formatSize(selectedSummary.bytes)}</strong>）
              </span>
              {selectedSummary.adminCount > 0 && (
                <StatusBadge tone="caution" icon={<IconLock width={12} height={12} />}>
                  含 {selectedSummary.adminCount} 项系统项（合并一次 UAC 授权）
                </StatusBadge>
              )}
            </div>
            <div className="hero-toolbar-right">
              <label className="inline-check" title="对被系统占用的系统垃圾文件安排下次开机自动删除">
                <input
                  type="checkbox"
                  checked={scheduleRebootDelete}
                  onChange={(event) => setScheduleRebootDelete(event.target.checked)}
                />
                <span>占用系统文件重启后删除</span>
              </label>
              <button type="button" className="btn-link" onClick={() => applyPreset("recommended")}>
                默认推荐
              </button>
              <button type="button" className="btn-link" onClick={() => applyPreset("all")}>
                全选
              </button>
              <button type="button" className="btn-link" onClick={() => applyPreset("none")}>
                清空
              </button>
            </div>
          </div>
        )}
      </div>

      {error && <AlertBanner tone="danger">{error}</AlertBanner>}
      {closeNotice && <AlertBanner tone="info">{closeNotice}</AlertBanner>}

      {/* 清理完成总结卡 */}
      {cleanReport && (
        <div className="card report-card">
          <div className="card-header">
            <div className="card-title-group">
              <StatusBadge tone={cleanReport.cancelled ? "caution" : "safe"} icon={<IconCheck width={12} height={12} />}>
                {cleanReport.cancelled ? "已停止" : "清理完成"}
              </StatusBadge>
              <h3 className="card-title">
                本次清理估算释放 {formatSize(cleanReport.freedBytes)}
                {actualFreed > 0 && `（C 盘实际可用增加 ${formatSize(actualFreed)}）`}
              </h3>
            </div>
            <div className="actions">
              <button
                type="button"
                className="btn btn-subtle"
                onClick={() => setReportExpanded((prev) => !prev)}
              >
                {reportExpanded ? "收起明细" : `查看 ${cleanReport.items.length} 项明细`}
              </button>
              <button type="button" className="btn btn-primary" onClick={startScan}>
                再次扫描
              </button>
            </div>
          </div>

          <div className="stat-grid">
            <div className="stat-box">
              <span className="stat-label">已删除文件</span>
              <strong className="stat-value td-num">{cleanReport.deletedFiles.toLocaleString()}</strong>
            </div>
            <div className="stat-box">
              <span className="stat-label">移入回收站（可还原）</span>
              <strong className="stat-value td-num">{cleanReport.recycledItems.toLocaleString()}</strong>
            </div>
            <div className="stat-box">
              <span className="stat-label">占用跳过</span>
              <strong className="stat-value td-num">{cleanReport.skippedFiles.toLocaleString()}</strong>
            </div>
            <div className="stat-box">
              <span className="stat-label">已排入重启删除</span>
              <strong className="stat-value td-num">{cleanReport.rebootScheduled.toLocaleString()}</strong>
            </div>
          </div>

          {cleanReport.recycledItems > 0 && (
            <p className="muted" style={{ marginTop: 10, marginBottom: 0 }}>
              提示：卸载残留项已安全移入 Windows 回收站，确认软件运行正常后清空回收站即可彻底释放空间。
            </p>
          )}

          {cleanReport.error && (
            <AlertBanner tone="danger">{cleanReport.error}</AlertBanner>
          )}

          {reportExpanded && (
            <div className="table-wrap" style={{ marginTop: 12 }}>
              <table className="table">
                <thead>
                  <tr>
                    <th>项目</th>
                    <th>状态</th>
                    <th>释放大小</th>
                    <th>说明</th>
                  </tr>
                </thead>
                <tbody>
                  {cleanReport.items.map((item) => (
                    <tr key={item.itemId}>
                      <td className="td-nowrap">{item.name}</td>
                      <td>
                        <StatusBadge
                          tone={
                            item.status === "done"
                              ? "safe"
                              : item.status === "partial"
                                ? "caution"
                                : item.status === "failed"
                                  ? "danger"
                                  : "neutral"
                          }
                        >
                          {item.status === "done"
                            ? "已完成"
                            : item.status === "partial"
                              ? "部分跳过"
                              : item.status === "failed"
                                ? "失败"
                                : "已跳过"}
                        </StatusBadge>
                      </td>
                      <td className="td-num">{formatSize(item.freedBytes)}</td>
                      <td>{item.detail}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      )}

      {/* 场景分类勾选树 */}
      {scanResult && (
        <div className="category-stack">
          {scanResult.categories.map((group) => {
            const isOpen = openCategories.has(group.category);
            const selectedInGroup = group.items.filter((item) => selected.has(item.id));
            const allChecked =
              group.items.length > 0 && selectedInGroup.length === group.items.length;
            const someChecked =
              selectedInGroup.length > 0 && selectedInGroup.length < group.items.length;
            const selectedBytesInGroup = selectedInGroup.reduce(
              (sum, item) => sum + (item.sizeBytes ?? 0),
              0,
            );

            return (
              <div className="card category-card" key={group.category}>
                <div
                  className="category-header"
                  onClick={() => toggleCategoryOpen(group.category)}
                  role="button"
                  tabIndex={0}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault();
                      toggleCategoryOpen(group.category);
                    }
                  }}
                >
                  <div className="category-header-left">
                    <IndeterminateCheckbox
                      checked={allChecked}
                      indeterminate={someChecked}
                      disabled={group.items.length === 0 || busy}
                      ariaLabel={`全选 ${group.label}`}
                      onChange={() => toggleCategorySelect(group)}
                    />
                    <span className="category-icon">{CATEGORY_ICON[group.category]}</span>
                    <div>
                      <div className="category-title-row">
                        <span className="category-title">{group.label}</span>
                        <span className="muted td-num">（{group.items.length} 项）</span>
                      </div>
                      <span className="category-desc">{group.description}</span>
                    </div>
                  </div>
                  <div className="category-header-right">
                    <span className="td-num category-size">
                      {selectedInGroup.length > 0 && (
                        <strong className="accent-text">{formatSize(selectedBytesInGroup)} / </strong>
                      )}
                      {formatSize(group.totalBytes)}
                    </span>
                    <IconChevron open={isOpen} width={16} height={16} />
                  </div>
                </div>

                {isOpen && (
                  <div className="category-body">
                    {group.items.length === 0 ? (
                      <p className="muted empty-inline">这一分类下未检测到可清理垃圾，很干净。</p>
                    ) : (
                      <div className="item-list">
                        {group.items.map((item) => {
                          const isChecked = selected.has(item.id);
                          const isExpanded = expandedItems.has(item.id);
                          const badge = sourceTag(item);
                          return (
                            <Fragment key={item.id}>
                              <div className={isChecked ? "clean-row clean-row-checked" : "clean-row"}>
                                <input
                                  type="checkbox"
                                  className="checkbox"
                                  checked={isChecked}
                                  disabled={busy}
                                  aria-label={`选择 ${item.name}`}
                                  onChange={() => toggleItem(item.id)}
                                />
                                <div className="clean-row-main">
                                  <div className="clean-row-title-line">
                                    <span className="clean-row-name">{item.name}</span>
                                    {badge && (
                                      <StatusBadge tone={badge.tone}>{badge.label}</StatusBadge>
                                    )}
                                    {item.requiresAdmin && (
                                      <StatusBadge tone="caution" icon={<IconLock width={11} height={11} />}>
                                        管理员
                                      </StatusBadge>
                                    )}
                                    {item.runningProcesses.length > 0 && (
                                      <StatusBadge tone="caution">
                                        运行中：{item.runningProcesses.join("、")}
                                      </StatusBadge>
                                    )}
                                  </div>
                                  <div className="clean-row-sub">
                                    <span>{item.description}</span>
                                    {item.warning && (
                                      <span className="warn-inline">
                                        <IconWarning width={12} height={12} /> {item.warning}
                                      </span>
                                    )}
                                    {item.lastModifiedAt !== null && (
                                      <span className="muted">
                                        最后修改：{formatTime(item.lastModifiedAt)}
                                      </span>
                                    )}
                                  </div>
                                </div>

                                <div className="clean-row-right">
                                  <div className="clean-row-size td-num">
                                    {item.sizeBytes !== null ? (
                                      <strong>{formatSize(item.sizeBytes)}</strong>
                                    ) : (
                                      <span className="muted">需提权查看</span>
                                    )}
                                    {item.fileCount > 0 && (
                                      <span className="muted file-count">
                                        {item.fileCount.toLocaleString()} 个文件
                                      </span>
                                    )}
                                  </div>
                                  <div className="clean-row-ops">
                                    {item.samplePaths.length > 0 && (
                                      <button
                                        type="button"
                                        className="btn-link"
                                        disabled={busy}
                                        onClick={() => toggleItemExpand(item.id)}
                                      >
                                        {isExpanded ? "收起" : "详情"}
                                      </button>
                                    )}
                                    {item.anchors.length > 0 && (
                                      <button
                                        type="button"
                                        className="btn-link"
                                        disabled={busy}
                                        title="在资源管理器中打开目录"
                                        onClick={() =>
                                          openCleanItemLocation(scanResult.scanId, item.id, null).catch(
                                            (reason) => setError(String(reason)),
                                          )
                                        }
                                      >
                                        <IconFolder width={14} height={14} />
                                      </button>
                                    )}
                                  </div>
                                </div>
                              </div>

                              {isExpanded && item.samplePaths.length > 0 && (
                                <div className="sample-box">
                                  <div className="sample-title">
                                    代表性路径（最多显示 {item.samplePaths.length} 条）
                                  </div>
                                  <ul className="sample-list">
                                    {item.samplePaths.map((sample, idx) => (
                                      <li key={`${item.id}-sample-${idx}`}>
                                        <button
                                          type="button"
                                          className="sample-path-btn"
                                          disabled={busy}
                                          onClick={() =>
                                            openCleanItemLocation(
                                              scanResult.scanId,
                                              item.id,
                                              idx,
                                            ).catch((reason) => setError(String(reason)))
                                          }
                                        >
                                          {sample}
                                        </button>
                                      </li>
                                    ))}
                                  </ul>
                                </div>
                              )}
                            </Fragment>
                          );
                        })}
                      </div>
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}

      {/* 系统深度瘦身大户卡片区 */}
      <div className="section-block">
        <div className="section-header">
          <div>
            <h3 className="section-title">系统深度瘦身（GB 级空间大户）</h3>
            <p className="muted">
              针对 C 盘最常见的超大系统占用项，单次 UAC 提权安全执行；若清理后 C 盘仍紧张，可前往{" "}
              <button type="button" className="btn-link" onClick={onOpenSpace}>
                空间分析定位大文件
              </button>
              。
            </p>
          </div>
          <button
            type="button"
            className="btn btn-subtle"
            disabled={busy}
            onClick={refreshBase}
          >
            <IconRefresh width={14} height={14} />
            <span>刷新状态</span>
          </button>
        </div>

        {slimProgress && (
          <AlertBanner tone="info">
            <div className="slim-progress-banner">
              <span>{slimProgress.detail}</span>
              {slimProgress.percent !== null && (
                <strong className="td-num">{slimProgress.percent.toFixed(1)}%</strong>
              )}
            </div>
          </AlertBanner>
        )}

        {slimResult && (
          <AlertBanner
            tone={
              slimResult.status === "done"
                ? "success"
                : slimResult.status === "failed"
                  ? "danger"
                  : "warn"
            }
          >
            {slimResult.detail}
            {slimResult.freeAfter > slimResult.freeBefore && (
              <>
                {" "}
                · C 盘可用空间增加{" "}
                <strong>{formatSize(slimResult.freeAfter - slimResult.freeBefore)}</strong>
              </>
            )}
          </AlertBanner>
        )}

        <div className="slim-grid">
          {/* 1. 休眠文件 */}
          <div className="card slim-card">
            <div className="slim-card-head">
              <div>
                <h4 className="slim-card-title">休眠文件 (hiberfil.sys)</h4>
                <span className="muted">
                  当前状态：{deepSlim ? hibernateModeLabel(deepSlim.hibernate.mode) : "读取中…"}
                </span>
              </div>
              <strong className="slim-size td-num">
                {deepSlim?.hibernate.fileBytes !== null && deepSlim?.hibernate.fileBytes !== undefined
                  ? formatSize(deepSlim.hibernate.fileBytes)
                  : deepSlim?.hibernate.mode === "off"
                    ? "0 B"
                    : "—"}
              </strong>
            </div>
            <p className="slim-desc">
              台式机或很少使用“休眠”时可精简或关闭，通常一次可释放 6–32 GB 空间。
            </p>
            <div className="slim-actions">
              {deepSlim?.hibernate.mode !== "off" && (
                <>
                  <button
                    type="button"
                    className="btn btn-primary btn-sm"
                    disabled={busy}
                    onClick={() =>
                      runDeepSlim(
                        "hibernateOff",
                        "关闭休眠会删除 C:\\hiberfil.sys 释放全部空间，同时停用快速启动和系统休眠（不影响睡眠）。可随时在此重新开启。确认继续？",
                      )
                    }
                  >
                    关闭休眠
                  </button>
                  {deepSlim?.hibernate.mode !== "reduced" && (
                    <button
                      type="button"
                      className="btn btn-subtle btn-sm"
                      disabled={busy}
                      onClick={() =>
                        runDeepSlim(
                          "hibernateReduced",
                          "精简模式会保留快速启动并释放约一半的休眠文件空间（同时会隐藏电源菜单中的“休眠”选项，不影响睡眠）。确认继续？",
                        )
                      }
                    >
                      精简约一半
                    </button>
                  )}
                </>
              )}
              {deepSlim?.hibernate.mode !== "full" && (
                <button
                  type="button"
                  className="btn btn-subtle btn-sm"
                  disabled={busy}
                  onClick={() => runDeepSlim("hibernateOn")}
                >
                  恢复完整开启
                </button>
              )}
            </div>
          </div>

          {/* 2. WinSxS 组件存储 */}
          <div className="card slim-card">
            <div className="slim-card-head">
              <div>
                <h4 className="slim-card-title">WinSxS 系统组件清理</h4>
                <span className="muted">
                  {deepSlim?.winsxs.reclaimableText
                    ? `可回收：${deepSlim.winsxs.reclaimableText}`
                    : "使用官方 DISM 清理已取代的旧系统组件"}
                </span>
              </div>
              <StatusBadge tone="safe" icon={<IconShield width={11} height={11} />}>
                保留回滚
              </StatusBadge>
            </div>
            <p className="slim-desc">
              {deepSlim?.winsxs.detail ??
                "安全清理过期的 Windows 更新备份组件（不加 /ResetBase，已装补丁仍可卸载）。"}
            </p>
            <div className="slim-actions">
              <button
                type="button"
                className="btn btn-subtle btn-sm"
                disabled={busy}
                onClick={() => runDeepSlim("dismAnalyze")}
              >
                分析可回收空间
              </button>
              <button
                type="button"
                className="btn btn-primary btn-sm"
                disabled={busy}
                onClick={() =>
                  runDeepSlim(
                    "dismCleanup",
                    "将调用官方 DISM /StartComponentCleanup 清理过期系统组件，耗时约 3–15 分钟，期间请勿关机。确认开始？",
                  )
                }
              >
                开始安全清理
              </button>
            </div>
          </div>

          {/* 3. VSS 系统还原点/卷影 */}
          <div className="card slim-card">
            <div className="slim-card-head">
              <div>
                <h4 className="slim-card-title">系统还原点与卷影副本</h4>
                <span className="muted">
                  {deepSlim?.vss.shadowCount !== null && deepSlim?.vss.shadowCount !== undefined
                    ? `已检测 ${deepSlim.vss.shadowCount} 个卷影`
                    : "需管理员查询精确数量"}
                </span>
              </div>
              <strong className="slim-size td-num">
                {deepSlim?.vss.usedBytes !== null && deepSlim?.vss.usedBytes !== undefined
                  ? formatSize(deepSlim.vss.usedBytes)
                  : "—"}
              </strong>
            </div>
            <p className="slim-desc">
              系统更新与安装驱动会自动创建历史还原点，逐个清理最旧卷影可温和回收数 GB。
            </p>
            <div className="slim-actions">
              <button
                type="button"
                className="btn btn-subtle btn-sm"
                disabled={busy}
                onClick={() =>
                  runDeepSlim(
                    "vssCleanOldest",
                    "将删除 C 盘最旧的一个系统卷影/还原点以释放空间，不影响最新还原点。确认继续？",
                  )
                }
              >
                清理最旧卷影副本
              </button>
            </div>
          </div>

          {/* 4. WSL2 / Docker 虚拟盘压缩 */}
          <div className="card slim-card">
            <div className="slim-card-head">
              <div>
                <h4 className="slim-card-title">WSL2 / Docker 虚拟盘压缩</h4>
                <span className="muted">
                  {deepSlim
                    ? deepSlim.wslVhdx.length > 0
                      ? `检测到 ${deepSlim.wslVhdx.length} 个虚拟盘`
                      : "未检测到 WSL / Docker 虚拟盘"
                    : "读取中…"}
                </span>
              </div>
              <strong className="slim-size td-num">
                {deepSlim && deepSlim.wslVhdx.length > 0
                  ? formatSize(
                      deepSlim.wslVhdx.reduce((sum, item) => sum + item.sizeBytes, 0),
                    )
                  : "0 B"}
              </strong>
            </div>
            <p className="slim-desc">
              WSL2 和 Docker 删除容器/文件后 .vhdx 不会自动缩容，压缩可把内部空洞还给 C 盘。
            </p>
            {deepSlim && deepSlim.wslVhdx.length > 0 && (
              <ul className="mini-list">
                {deepSlim.wslVhdx.slice(0, 3).map((disk) => (
                  <li key={disk.path} className="td-num">
                    <span className="truncate">{disk.name}</span>
                    <span>{formatSize(disk.sizeBytes)}</span>
                  </li>
                ))}
              </ul>
            )}
            <div className="slim-actions">
              <button
                type="button"
                className="btn btn-subtle btn-sm"
                disabled={busy || !deepSlim || deepSlim.wslVhdx.length === 0}
                onClick={() =>
                  runDeepSlim(
                    "wslCompact",
                    "压缩前会先执行 wsl --shutdown 暂停 WSL 实例，再调用 Windows 虚拟磁盘 API 回收空洞。若正在运行 Docker Desktop，建议先手动退出 Docker Desktop 以防自动拉起虚拟盘。确认继续？",
                  )
                }
              >
                压缩虚拟盘
              </button>
            </div>
          </div>

          {/* 5. 虚拟内存状态卡 */}
          <div className="card slim-card">
            <div className="slim-card-head">
              <div>
                <h4 className="slim-card-title">虚拟内存 (pagefile.sys)</h4>
                <span className="muted">只读状态检测</span>
              </div>
              <strong className="slim-size td-num">
                {deepSlim && deepSlim.pagefiles.length > 0
                  ? formatSize(
                      deepSlim.pagefiles.reduce((sum, p) => sum + (p.sizeBytes ?? 0), 0),
                    )
                  : "系统托管"}
              </strong>
            </div>
            <p className="slim-desc">
              {deepSlim?.pagefiles.some((p) => p.onSystemDrive)
                ? "当前虚拟内存位于 C 盘。如需释放此空间，可在「系统属性 → 高级 → 性能设置 → 虚拟内存」中迁移至其他 SSD。"
                : "虚拟内存已不在 C 盘或处于系统自动管理状态，无需手动改动。"}
            </p>
          </div>
        </div>
      </div>

      {/* 进程冲突提示弹窗 */}
      {conflicts && (
        <div
          className="modal-backdrop"
          role="dialog"
          aria-modal="true"
          aria-labelledby="conflict-title"
          onClick={() => {
            if (!closingProcs && !recheckingProcs) setConflicts(null);
          }}
        >
          <div className="modal-card" onClick={(event) => event.stopPropagation()}>
            <div className="modal-header">
              <IconWarning width={20} height={20} className="warn-icon" />
              <h3 id="conflict-title" className="modal-title">
                检测到正在运行的软件
              </h3>
            </div>
            <p className="modal-desc">
              以下软件正在后台运行，关闭后可以更彻底地清理其缓存；你也可以手动关闭后点「重新检测」，或直接跳过被占用的单个文件继续清理。
            </p>
            <ul className="conflict-list">
              {conflicts.map((conflict) => (
                <li key={conflict.appName} className="conflict-row">
                  <div>
                    <strong>{conflict.appName}</strong>
                    <span className="muted">（{conflict.processNames.join("、")}）</span>
                  </div>
                  <span className="td-num">{formatSize(conflict.affectedBytes)}</span>
                </li>
              ))}
            </ul>
            <div className="modal-actions">
              <button
                type="button"
                className="btn btn-primary"
                disabled={closingProcs || recheckingProcs}
                onClick={handleGracefulCloseAndClean}
              >
                {closingProcs ? "正在请求关闭…" : "优雅关闭并清理"}
              </button>
              <button
                type="button"
                className="btn btn-subtle"
                disabled={closingProcs || recheckingProcs}
                onClick={handleRecheckProcesses}
              >
                {recheckingProcs ? "检测中…" : "已关闭 · 重新检测"}
              </button>
              <button
                type="button"
                className="btn btn-subtle"
                disabled={closingProcs || recheckingProcs}
                onClick={() => runCleanNow(Array.from(selected))}
              >
                跳过占用直接清理
              </button>
              <button
                type="button"
                className="btn btn-subtle"
                disabled={closingProcs || recheckingProcs}
                onClick={() => setConflicts(null)}
              >
                取消
              </button>
            </div>
          </div>
        </div>
      )}
    </section>
  );
}
