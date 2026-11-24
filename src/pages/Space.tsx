import { useEffect, useState } from "react";
import {
  defaultScanRoots,
  spaceOpenInExplorer,
  spaceRecycleItem,
  spaceScanCancel,
  spaceScanChildren,
  spaceScanStart,
  spaceScanSummary,
  spaceScanTopFiles,
} from "../api";
import { IconFolder, IconSearch, IconSpace, IconTrash, IconZap } from "../components/Icons";
import { Treemap } from "../components/Treemap";
import { AlertBanner, EmptyState, StatusBadge } from "../components/Ui";
import { formatDuration, formatSize, formatTime } from "../format";
import type { DirNode, FileEntry, ScanProgress, ScanRoot, ScanSummary } from "../types";

const TOP_FILE_LIMIT = 200;

type Tab = "dirs" | "graph" | "files";

export function SpacePage({ onRecycled }: { onRecycled: () => void }) {
  const [roots, setRoots] = useState<ScanRoot[]>([]);
  const [selectedPath, setSelectedPath] = useState("C:\\");
  const [useMft, setUseMft] = useState(false);

  const [scanning, setScanning] = useState(false);
  const [progress, setProgress] = useState<ScanProgress | null>(null);
  const [summary, setSummary] = useState<ScanSummary | null>(null);

  const [tab, setTab] = useState<Tab>("dirs");
  const [trail, setTrail] = useState<DirNode[]>([]);
  const [rows, setRows] = useState<DirNode[]>([]);
  const [files, setFiles] = useState<FileEntry[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [recyclingKey, setRecyclingKey] = useState<string | null>(null);

  const currentId = trail.length > 0 ? trail[trail.length - 1].id : null;

  useEffect(() => {
    defaultScanRoots()
      .then((list) => {
        setRoots(list);
        if (list.length > 0) {
          setSelectedPath(list[0].path);
        }
      })
      .catch(() => {});

    spaceScanSummary()
      .then((saved) => {
        if (saved) setSummary(saved);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!summary || tab === "files") return;
    spaceScanChildren(currentId)
      .then(setRows)
      .catch((reason) => setError(String(reason)));
  }, [summary, tab, currentId]);

  useEffect(() => {
    if (!summary || tab !== "files") return;
    spaceScanTopFiles(TOP_FILE_LIMIT)
      .then(setFiles)
      .catch((reason) => setError(String(reason)));
  }, [summary, tab]);

  const startScan = (mftOverride?: boolean) => {
    const trimmed = selectedPath.trim();
    if (!trimmed || scanning) return;
    const modeMft = mftOverride ?? useMft;
    setError(null);
    setNotice(null);
    setScanning(true);
    setTrail([]);
    setProgress({
      dirs: 0,
      files: 0,
      allocated: 0,
      elapsedMs: 0,
      mode: modeMft ? "mft" : "standard",
      done: false,
      cancelled: false,
      error: null,
    });

    spaceScanStart(trimmed, modeMft, (next) => setProgress(next))
      .then((result) => {
        setSummary(result);
        setTrail([]);
        if (tab === "files") {
          spaceScanTopFiles(TOP_FILE_LIMIT)
            .then(setFiles)
            .catch(() => {});
        } else {
          spaceScanChildren(null)
            .then(setRows)
            .catch(() => {});
        }
      })
      .catch((reason) => setError(String(reason)))
      .finally(() => {
        setScanning(false);
        setProgress(null);
      });
  };

  const handleRecycle = async (kind: "node" | "topFile", id: number, label: string, size: number) => {
    const confirmed = window.confirm(
      `确定将「${label}」（约 ${formatSize(size)}）移入 Windows 回收站吗？\n\n系统核心保护目录会被后端自动拦截；移入回收站后如需恢复可在回收站还原。`,
    );
    if (!confirmed) return;

    const key = `${kind}-${id}`;
    setRecyclingKey(key);
    setError(null);
    setNotice(null);
    try {
      const result = await spaceRecycleItem(kind, id);
      setNotice(`${result.detail}（${result.path}）· 可重新扫描刷新最新目录大小`);
      if (kind === "node") {
        setRows((prev) => prev.filter((item) => item.id !== id));
      } else {
        setFiles((prev) => prev.filter((item) => item.index !== id));
      }
      onRecycled();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setRecyclingKey(null);
    }
  };

  const spaceBusy = scanning || recyclingKey !== null;

  return (
    <section className="page-section">
      <div className="page-header">
        <div>
          <h2 className="page-title">空间分析</h2>
          <p className="page-subtitle">快速定位 C 盘超大文件夹与隐藏大文件，支持直达目录或安全移入回收站</p>
        </div>
      </div>

      {/* 扫描控制卡片 */}
      <div className="card space-control-card">
        <div className="space-roots-row">
          {roots.map((root) => (
            <button
              key={root.path}
              type="button"
              className={
                selectedPath === root.path ? "chip chip-active" : "chip"
              }
              disabled={scanning}
              onClick={() => setSelectedPath(root.path)}
            >
              {root.label}
            </button>
          ))}
        </div>

        <div className="space-input-row">
          <input
            type="text"
            className="text-input"
            value={selectedPath}
            disabled={scanning}
            placeholder="输入要分析的目录，例如 C:\ 或 C:\Users"
            onChange={(event) => setSelectedPath(event.target.value)}
          />
          <label className="inline-check mft-toggle" title="仅支持 NTFS 卷根目录（如 C:\），需要一次 UAC 管理员授权，1~3 秒扫完全盘">
            <input
              type="checkbox"
              checked={useMft}
              disabled={scanning}
              onChange={(event) => setUseMft(event.target.checked)}
            />
            <IconZap width={14} height={14} />
            <span>NTFS MFT 极速扫描（管理员）</span>
          </label>
          {scanning ? (
            <button
              type="button"
              className="btn btn-danger"
              onClick={() => spaceScanCancel().catch(() => {})}
            >
              取消扫描
            </button>
          ) : (
            <button type="button" className="btn btn-primary" onClick={() => startScan()}>
              <IconSearch width={15} height={15} />
              <span>开始分析</span>
            </button>
          )}
        </div>

        {scanning && progress && (
          <div className="live-progress">
            <div className="live-progress-row td-num">
              <span className="live-phase">
                {progress.mode === "mft" ? "MFT 极速解析中…" : "并发目录扫描中…"}
              </span>
              <span className="muted">
                已扫描 {progress.dirs.toLocaleString()} 个目录 · {progress.files.toLocaleString()} 个文件
              </span>
              <span className="live-bytes">{formatSize(progress.allocated)}</span>
            </div>
            <div className="bar-indeterminate">
              <div className="bar-indeterminate-inner" />
            </div>
          </div>
        )}
      </div>

      {error && <AlertBanner tone="danger">{error}</AlertBanner>}
      {notice && <AlertBanner tone="success">{notice}</AlertBanner>}
      {summary?.fallbackNote && <AlertBanner tone="info">{summary.fallbackNote}</AlertBanner>}

      {!summary && !scanning && (
        <div className="card">
          <EmptyState
            icon={<IconSpace width={36} height={36} />}
            title="选择起点开始分析 C 盘空间"
            description="点击「开始分析」扫描全盘占用；勾选「NTFS MFT 极速扫描」可在 1–3 秒内完成整块 C 盘统计。"
          />
        </div>
      )}

      {summary && (
        <>
          <div className="space-summary-bar">
            <div className="td-num space-summary-meta">
              <StatusBadge tone={summary.mode === "mft" ? "safe" : "info"}>
                {summary.mode === "mft" ? "MFT 极速扫描" : "标准并发扫描"}
              </StatusBadge>
              <span>
                起点 <strong>{summary.root}</strong> · 占盘{" "}
                <strong>{formatSize(summary.allocated)}</strong> ·{" "}
                {summary.files.toLocaleString()} 个文件 · 耗时 {formatDuration(summary.elapsedMs)}
              </span>
              {summary.deniedDirs > 0 && (
                <span className="muted">（{summary.deniedDirs} 个受保护目录无权访问）</span>
              )}
            </div>

            <div className="tabs" role="tablist" aria-label="空间分析视图">
              <button
                type="button"
                role="tab"
                aria-selected={tab === "dirs"}
                className={tab === "dirs" ? "tab tab-active" : "tab"}
                onClick={() => setTab("dirs")}
              >
                目录树
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={tab === "graph"}
                className={tab === "graph" ? "tab tab-active" : "tab"}
                onClick={() => setTab("graph")}
              >
                占比图
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={tab === "files"}
                className={tab === "files" ? "tab tab-active" : "tab"}
                onClick={() => setTab("files")}
              >
                大文件 Top {TOP_FILE_LIMIT}
              </button>
            </div>
          </div>

          {tab !== "files" && (
            <nav className="crumbs" aria-label="当前目录层级">
              <button type="button" className="crumb-btn" onClick={() => setTrail([])}>
                {summary.root}
              </button>
              {trail.map((node, index) => (
                <span className="crumb-item" key={node.id}>
                  <span className="crumb-sep">/</span>
                  <button
                    type="button"
                    className="crumb-btn"
                    onClick={() => setTrail(trail.slice(0, index + 1))}
                  >
                    {node.name}
                  </button>
                </span>
              ))}
            </nav>
          )}

          {tab === "graph" && (
            <div className="card">
              <div className="card-header">
                <h3 className="card-title">子目录占比图</h3>
                <span className="muted">点击目录块可进入下一级</span>
              </div>
              <Treemap nodes={rows} onEnter={(node) => setTrail([...trail, node])} />
            </div>
          )}

          {tab === "dirs" && (
            <div className="card">
              <div className="table-wrap">
                <table className="table">
                  <thead>
                    <tr>
                      <th>目录名</th>
                      <th>占盘大小</th>
                      <th>文件数</th>
                      <th>占比</th>
                      <th>操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {rows.length === 0 && (
                      <tr>
                        <td colSpan={5} className="muted">
                          此目录下没有可展示的子目录
                        </td>
                      </tr>
                    )}
                    {rows.map((node) => (
                      <tr key={node.id}>
                        <td>
                          {node.hasChildren ? (
                            <button
                              type="button"
                              className="btn-link"
                              disabled={spaceBusy}
                              onClick={() => setTrail([...trail, node])}
                            >
                              <IconFolder width={14} height={14} />
                              <span>{node.name}</span>
                            </button>
                          ) : (
                            <span>{node.name}</span>
                          )}
                          {node.denied && <StatusBadge tone="caution">部分受限</StatusBadge>}
                        </td>
                        <td className="td-num">
                          <strong>{formatSize(node.allocated)}</strong>
                        </td>
                        <td className="td-num">{node.files.toLocaleString()}</td>
                        <td style={{ width: 140 }}>
                          <div
                            className="bar bar-thin"
                            title={`${(node.percentOfParent * 100).toFixed(1)}%`}
                          >
                            <div
                              className="bar-fill"
                              style={{ width: `${Math.min(node.percentOfParent, 1) * 100}%` }}
                            />
                          </div>
                        </td>
                        <td className="td-nowrap">
                          <div className="actions">
                            <button
                              type="button"
                              className="btn-link"
                              disabled={spaceBusy}
                              onClick={() =>
                                spaceOpenInExplorer("node", node.id).catch((reason) =>
                                  setError(String(reason)),
                                )
                              }
                            >
                              打开位置
                            </button>
                            <button
                              type="button"
                              className="btn-link danger-link"
                              disabled={spaceBusy}
                              onClick={() =>
                                handleRecycle("node", node.id, node.name, node.allocated)
                              }
                            >
                              <IconTrash width={13} height={13} />
                              <span>移入回收站</span>
                            </button>
                          </div>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}

          {tab === "files" && (
            <div className="card">
              <div className="table-wrap">
                <table className="table">
                  <thead>
                    <tr>
                      <th>文件路径</th>
                      <th>占盘大小</th>
                      <th>最后修改</th>
                      <th>操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {files.length === 0 && (
                      <tr>
                        <td colSpan={4} className="muted">
                          没有找到大文件记录
                        </td>
                      </tr>
                    )}
                    {files.map((file) => (
                      <tr key={file.index}>
                        <td className="path-cell" title={file.path}>
                          {file.path}
                        </td>
                        <td className="td-num td-nowrap">
                          <strong>{formatSize(file.allocated)}</strong>
                        </td>
                        <td className="td-num td-nowrap">{formatTime(file.lastWrite)}</td>
                        <td className="td-nowrap">
                          <div className="actions">
                            <button
                              type="button"
                              className="btn-link"
                              disabled={spaceBusy}
                              onClick={() =>
                                spaceOpenInExplorer("topFile", file.index).catch((reason) =>
                                  setError(String(reason)),
                                )
                              }
                            >
                              打开位置
                            </button>
                            <button
                              type="button"
                              className="btn-link danger-link"
                              disabled={spaceBusy}
                              onClick={() =>
                                handleRecycle("topFile", file.index, file.path, file.allocated)
                              }
                            >
                              <IconTrash width={13} height={13} />
                              <span>移入回收站</span>
                            </button>
                          </div>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}
        </>
      )}
    </section>
  );
}
