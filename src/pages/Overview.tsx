// 总览页：C 盘用量、其他固定卷、只读环境状态与全盘空间扫描入口。
import { useEffect, useState } from "react";
import {
  currentScanProgress,
  defaultScanRoots,
  envState,
  scanCancel,
  scanStart,
  scanSummary,
  subscribeScan,
  volumeInfo,
} from "../api";
import { IconFolder, IconSearch } from "../components/Icons";
import { AlertBanner, StatusBadge } from "../components/Ui";
import { formatDuration, formatSize } from "../format";
import type {
  EnvLocation,
  EnvState,
  ScanProgress,
  ScanRoot,
  ScanSummary,
  VolumeInfo,
} from "../types";

export function OverviewPage() {
  const [volumes, setVolumes] = useState<VolumeInfo[]>([]);
  const [roots, setRoots] = useState<ScanRoot[]>([]);
  const [summary, setSummary] = useState<ScanSummary | null>(null);
  const [progress, setProgress] = useState<ScanProgress | null>(currentScanProgress());
  const [env, setEnv] = useState<EnvState | null>(null);
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
    // 环境状态卡只是只读信息，读不到就整块不显示，不影响扫描。
    envState()
      .then(setEnv)
      .catch(() => setEnv(null));

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
  const usedPercent = Math.round(usedRatio * 100);

  const radius = 58;
  const circumference = 2 * Math.PI * radius;
  const strokeDashoffset = circumference * (1 - usedRatio);
  const ringClass =
    usedPercent >= 92
      ? "ring-fill ring-fill-crit"
      : usedPercent >= 80
        ? "ring-fill ring-fill-warn"
        : "ring-fill";

  const startScan = (root: string) => {
    setError(null);
    // 新扫描会替换后端保存的结果，旧摘要留着只会误导。
    setSummary(null);
    scanStart(root).catch((reason) => setError(String(reason)));
  };

  return (
    <section className="page-section">
      <div className="page-header">
        <div>
          <h2 className="page-title">总览</h2>
          <p className="page-subtitle">系统盘容量状态、只读环境体检与全盘空间扫描</p>
        </div>
      </div>

      <div className="overview-hero">
        <div className="card">
          <div className="card-header">
            <h3 className="card-title">系统卷容量</h3>
            {systemVolume && (
              <StatusBadge tone={usedPercent >= 90 ? "danger" : usedPercent >= 78 ? "caution" : "safe"}>
                {usedPercent >= 90 ? "空间紧张" : usedPercent >= 78 ? "建议关注" : "空间充裕"}
              </StatusBadge>
            )}
          </div>

          {!systemVolume && <p className="muted">没有读到磁盘信息</p>}

          {systemVolume && (
            <div className="volume-hero">
              <div className="ring-wrap">
                <svg
                  className="ring-svg"
                  width="148"
                  height="148"
                  viewBox="0 0 148 148"
                  role="img"
                  aria-label="系统盘使用情况"
                >
                  <circle className="ring-track" cx="74" cy="74" r={radius} />
                  <circle
                    className={ringClass}
                    cx="74"
                    cy="74"
                    r={radius}
                    strokeDasharray={circumference}
                    strokeDashoffset={strokeDashoffset}
                  />
                </svg>
                <div className="ring-center">
                  <span className="ring-percent">{usedPercent}%</span>
                  <span className="ring-caption">已用</span>
                </div>
              </div>

              <div className="volume-meta">
                <p className="volume-title">
                  {systemVolume.root} {systemVolume.label || "本地磁盘"}
                  {systemVolume.isSystem ? "（系统盘）" : ""}
                </p>
                <p className="volume-free-hero">
                  剩余 <strong>{formatSize(systemVolume.free)}</strong> /{" "}
                  <span className="td-num">{formatSize(systemVolume.total)}</span>
                </p>

                <dl className="stats-grid">
                  <div className="stat-tile">
                    <dt>已用</dt>
                    <dd>{formatSize(usedBytes)}</dd>
                  </div>
                  <div className="stat-tile">
                    <dt>总计</dt>
                    <dd>{formatSize(systemVolume.total)}</dd>
                  </div>
                  <div className="stat-tile">
                    <dt>文件系统</dt>
                    <dd>{systemVolume.fs || "未知"}</dd>
                  </div>
                </dl>
              </div>
            </div>
          )}
        </div>

        <div className="card">
          <div className="card-header">
            <h3 className="card-title">扫描</h3>
            <span className="muted">只读统计，不修改文件</span>
          </div>
          <p className="card-lead">
            扫描完成后可在「空间分析」页查看目录树图与最大文件分布。
          </p>

          <div className="actions">
            {roots.map((root) => (
              <button
                key={root.path}
                type="button"
                className="btn btn-primary"
                disabled={scanning}
                onClick={() => startScan(root.path)}
              >
                <IconSearch width={14} height={14} />
                <span>扫描{root.label}</span>
              </button>
            ))}
            {scanning && (
              <button
                type="button"
                className="btn btn-subtle"
                onClick={() => scanCancel().catch((reason) => setError(String(reason)))}
              >
                取消扫描
              </button>
            )}
          </div>

          {error && (
            <div style={{ marginTop: 12 }}>
              <AlertBanner tone="danger">{error}</AlertBanner>
            </div>
          )}
          {progress?.error && (
            <div style={{ marginTop: 12 }}>
              <AlertBanner tone="danger">{progress.error}</AlertBanner>
            </div>
          )}

          {scanning && progress && (
            <div className="scan-live">
              <div className="scan-pulse-bar" />
              <dl className="stats-grid">
                <div className="stat-tile">
                  <dt>目录数</dt>
                  <dd>{progress.dirs.toLocaleString()}</dd>
                </div>
                <div className="stat-tile">
                  <dt>文件数</dt>
                  <dd>{progress.files.toLocaleString()}</dd>
                </div>
                <div className="stat-tile">
                  <dt>已统计大小</dt>
                  <dd>{formatSize(progress.allocated)}</dd>
                </div>
                <div className="stat-tile">
                  <dt>耗时</dt>
                  <dd>{formatDuration(progress.elapsedMs)}</dd>
                </div>
              </dl>
            </div>
          )}

          {summary && !scanning && (
            <div className="summary-box">
              <p className="summary-title">
                {summary.cancelled ? "扫描已取消" : "扫描完成"}：
                <span className="path-text">{summary.root}</span>
              </p>
              <dl className="stats-grid">
                <div className="stat-tile">
                  <dt>占盘大小</dt>
                  <dd>{formatSize(summary.allocated)}</dd>
                </div>
                <div className="stat-tile">
                  <dt>逻辑大小</dt>
                  <dd>{formatSize(summary.logical)}</dd>
                </div>
                <div className="stat-tile">
                  <dt>文件数</dt>
                  <dd>{summary.files.toLocaleString()}</dd>
                </div>
                <div className="stat-tile">
                  <dt>目录数</dt>
                  <dd>{summary.dirs.toLocaleString()}</dd>
                </div>
                <div className="stat-tile">
                  <dt>耗时</dt>
                  <dd>{formatDuration(summary.elapsedMs)}</dd>
                </div>
              </dl>
              {summary.cancelled && (
                <div style={{ marginTop: 10 }}>
                  <AlertBanner tone="warn">扫描中途取消，结果不完整。</AlertBanner>
                </div>
              )}
              {summary.deniedDirs > 0 && (
                <div style={{ marginTop: 10 }}>
                  <AlertBanner tone="warn">
                    {summary.deniedDirs.toLocaleString()} 个目录无权读取（普通权限扫描不完整）
                  </AlertBanner>
                </div>
              )}
            </div>
          )}
        </div>
      </div>

      {otherVolumes.length > 0 && (
        <div className="card">
          <div className="card-header">
            <h3 className="card-title">其他固定盘</h3>
            <span className="muted">可用作隔离区或目录迁移目标盘</span>
          </div>
          <div className="volume-list">
            {otherVolumes.map((volume) => {
              const used = volume.total - volume.free;
              const ratio = volume.total > 0 ? (used / volume.total) * 100 : 0;

              return (
                <div className="volume-item" key={volume.root}>
                  <div className="volume-item-head">
                    <span className="volume-item-name">
                      {volume.root} {volume.label || "本地磁盘"}
                    </span>
                    <span className="badge badge-neutral">{volume.fs || "未知"}</span>
                  </div>
                  <div className="bar">
                    <div className="bar-fill" style={{ width: `${ratio.toFixed(1)}%` }} />
                  </div>
                  <span className="volume-item-stat">
                    剩余 {formatSize(volume.free)} / 共 {formatSize(volume.total)}
                  </span>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {env && (
        <div className="card">
          <div className="card-header">
            <div className="card-title-group">
              <IconFolder />
              <h3 className="card-title">环境状态</h3>
            </div>
            <span className="muted">只读展示系统目录与虚拟磁盘当前位置</span>
          </div>

          <div className="env-grid">
            <div className="env-panel">
              <h4 className="env-panel-title">休眠与页面文件</h4>
              <div className="env-lines">
                <div className="env-line">
                  <span className="env-name">休眠</span>
                  <span className="env-path">
                    {env.hibernateEnabled === null
                      ? "未知"
                      : env.hibernateEnabled
                        ? "已开启"
                        : "已关闭"}
                  </span>
                </div>
                {locationLines(env.pagefiles, "没有配置页面文件")}
              </div>
            </div>

            <div className="env-panel">
              <h4 className="env-panel-title">用户目录与 OneDrive</h4>
              <div className="env-lines">
                {locationLines(env.knownFolders, "未知")}
                {env.onedrive
                  ? locationLines([env.onedrive], "")
                  : emptyLine("没有配置 OneDrive")}
              </div>
            </div>

            <div className="env-panel">
              <h4 className="env-panel-title">WSL 发行版</h4>
              <div className="env-lines">
                {locationLines(env.wslDistros, "没有安装 WSL 发行版")}
              </div>
            </div>

            <div className="env-panel">
              <h4 className="env-panel-title">未关联虚拟磁盘</h4>
              <div className="env-lines">
                {locationLines(env.unregisteredVhdx, "没有未关联的虚拟磁盘")}
              </div>
            </div>
          </div>
        </div>
      )}
    </section>
  );
}

// 一组位置项：读不到时显示“未知”，没有配置时显示给定文案。
function locationLines(locations: EnvLocation[] | null, emptyText: string) {
  if (locations === null) {
    return emptyLine("未知");
  }

  if (locations.length === 0) {
    return emptyLine(emptyText);
  }

  return locations.map((location, index) => (
    <div className="env-line" key={`${location.name ?? location.path ?? "未知"}-${index}`}>
      {location.name && <span className="env-name">{location.name}</span>}
      <span className="env-path" title={location.path ?? undefined}>
        {location.path ?? "未知"}
      </span>
      {location.sizeBytes !== null && (
        <span className="muted td-num">{formatSize(location.sizeBytes)}</span>
      )}
      <StatusBadge
        tone={
          location.onSystemDrive === true
            ? "caution"
            : location.onSystemDrive === false
              ? "safe"
              : "neutral"
        }
      >
        {driveLabel(location.onSystemDrive)}
      </StatusBadge>
    </div>
  ));
}

// 一行说明文字，用于“未知”“没有配置”这类占位。
function emptyLine(text: string) {
  return (
    <div className="env-line">
      <span className="muted">{text}</span>
    </div>
  );
}

// 统一用“在 C 盘 / 已在其他盘”的说法。
function driveLabel(onSystemDrive: boolean | null): string {
  if (onSystemDrive === null) {
    return "位置未知";
  }

  return onSystemDrive ? "在 C 盘" : "已在其他盘";
}
