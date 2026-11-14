import { useEffect, useState } from "react";
import {
  currentScanProgress,
  defaultScanRoots,
  scanCancel,
  scanStart,
  scanSummary,
  subscribeScan,
  volumeInfo,
} from "../api";
import { formatDuration, formatSize } from "../format";
import type { ScanProgress, ScanRoot, ScanSummary, VolumeInfo } from "../types";

const RING_RADIUS = 54;
const RING_STROKE = 14;
const RING_CIRCUMFERENCE = 2 * Math.PI * RING_RADIUS;

export function OverviewPage() {
  const [volumes, setVolumes] = useState<VolumeInfo[]>([]);
  const [roots, setRoots] = useState<ScanRoot[]>([]);
  const [summary, setSummary] = useState<ScanSummary | null>(null);
  const [progress, setProgress] = useState<ScanProgress | null>(currentScanProgress());
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    volumeInfo()
      .then(setVolumes)
      .catch((reason) => setError(String(reason)));
    defaultScanRoots()
      .then(setRoots)
      .catch((reason) => setError(String(reason)));
    scanSummary()
      .then(setSummary)
      .catch(() => setSummary(null));

    // 页面可能是在扫描途中被重新打开的，先用模块里缓存的最新进度恢复界面。
    setProgress(currentScanProgress());
    return subscribeScan(setProgress);
  }, []);

  // 扫描结束（含取消）后重新取一次摘要，摘要里的数字才是最终结果。
  useEffect(() => {
    if (!progress?.done || progress.error) {
      return;
    }
    scanSummary()
      .then(setSummary)
      .catch(() => setSummary(null));
  }, [progress?.done, progress?.error]);

  const scanning = progress !== null && !progress.done;
  const systemVolume =
    volumes.find((volume) => volume.root.toUpperCase() === "C:\\") ?? volumes[0] ?? null;
  const otherVolumes = volumes.filter((volume) => volume !== systemVolume);

  const usedBytes = systemVolume ? systemVolume.total - systemVolume.free : 0;
  const usedRatio =
    systemVolume && systemVolume.total > 0 ? usedBytes / systemVolume.total : 0;

  const startScan = (root: string) => {
    setError(null);
    // 新扫描会替换后端保存的结果，旧摘要留着只会误导。
    setSummary(null);
    scanStart(root).catch((reason) => setError(String(reason)));
  };

  return (
    <section>
      <h2>总览</h2>

      <div className="card disk-card">
        {systemVolume ? (
          <>
            <svg
              className="ring"
              viewBox="0 0 140 140"
              width="140"
              height="140"
              role="img"
              aria-label="系统盘使用情况"
            >
              <circle
                cx="70"
                cy="70"
                r={RING_RADIUS}
                fill="none"
                stroke="#e3e6ea"
                strokeWidth={RING_STROKE}
              />
              <circle
                cx="70"
                cy="70"
                r={RING_RADIUS}
                fill="none"
                stroke="#1a56db"
                strokeWidth={RING_STROKE}
                strokeLinecap="round"
                strokeDasharray={`${usedRatio * RING_CIRCUMFERENCE} ${RING_CIRCUMFERENCE}`}
                transform="rotate(-90 70 70)"
              />
              <text x="70" y="68" textAnchor="middle" className="ring-percent">
                {(usedRatio * 100).toFixed(0)}%
              </text>
              <text x="70" y="88" textAnchor="middle" className="ring-label">
                已用
              </text>
            </svg>

            <div className="disk-detail">
              <p className="disk-title">
                {systemVolume.root} {systemVolume.label || "本地磁盘"}
                {systemVolume.isSystem ? "（系统盘）" : ""} ·{" "}
                {systemVolume.fs || "未知文件系统"}
              </p>
              <p>已用 {formatSize(usedBytes)}</p>
              <p>剩余 {formatSize(systemVolume.free)}</p>
              <p>总计 {formatSize(systemVolume.total)}</p>
            </div>
          </>
        ) : (
          <p className="muted">没有读到磁盘信息</p>
        )}
      </div>

      {otherVolumes.length > 0 && (
        <div className="card">
          <h3>其他固定盘</h3>
          {otherVolumes.map((volume) => (
            <div className="volume-row" key={volume.root}>
              <span className="volume-name">
                {volume.root} {volume.label || "本地磁盘"}
              </span>
              <div className="bar">
                <div
                  className="bar-fill"
                  style={{
                    width: `${
                      volume.total > 0
                        ? ((volume.total - volume.free) / volume.total) * 100
                        : 0
                    }%`,
                  }}
                />
              </div>
              <span className="volume-size">
                剩余 {formatSize(volume.free)} / 共 {formatSize(volume.total)}
              </span>
            </div>
          ))}
        </div>
      )}

      <div className="card">
        <h3>扫描</h3>
        <div className="actions">
          {roots.map((root) => (
            <button
              key={root.path}
              type="button"
              className="button-primary"
              disabled={scanning}
              onClick={() => startScan(root.path)}
            >
              扫描{root.label}
            </button>
          ))}
          {scanning && (
            <button type="button" onClick={() => scanCancel().catch((reason) => setError(String(reason)))}>
              取消扫描
            </button>
          )}
        </div>

        {error && <p className="warn">{error}</p>}
        {progress?.error && <p className="warn">{progress.error}</p>}

        {scanning && progress && (
          <dl className="stats">
            <div>
              <dt>目录数</dt>
              <dd>{progress.dirs.toLocaleString()}</dd>
            </div>
            <div>
              <dt>文件数</dt>
              <dd>{progress.files.toLocaleString()}</dd>
            </div>
            <div>
              <dt>已统计大小</dt>
              <dd>{formatSize(progress.allocated)}</dd>
            </div>
            <div>
              <dt>耗时</dt>
              <dd>{formatDuration(progress.elapsedMs)}</dd>
            </div>
          </dl>
        )}

        {summary && !scanning && (
          <div className="summary">
            <p className="summary-title">
              {summary.cancelled ? "扫描已取消" : "扫描完成"}：{summary.root}
            </p>
            <dl className="stats">
              <div>
                <dt>占盘大小</dt>
                <dd>{formatSize(summary.allocated)}</dd>
              </div>
              <div>
                <dt>逻辑大小</dt>
                <dd>{formatSize(summary.logical)}</dd>
              </div>
              <div>
                <dt>文件数</dt>
                <dd>{summary.files.toLocaleString()}</dd>
              </div>
              <div>
                <dt>目录数</dt>
                <dd>{summary.dirs.toLocaleString()}</dd>
              </div>
              <div>
                <dt>耗时</dt>
                <dd>{formatDuration(summary.elapsedMs)}</dd>
              </div>
            </dl>
            {summary.cancelled && <p className="warn">扫描中途取消，结果不完整。</p>}
            {summary.deniedDirs > 0 && (
              <p className="warn">
                {summary.deniedDirs.toLocaleString()} 个目录无权读取（普通权限扫描不完整）
              </p>
            )}
          </div>
        )}
      </div>
    </section>
  );
}
