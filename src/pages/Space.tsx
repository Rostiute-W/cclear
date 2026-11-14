import { useEffect, useState } from "react";
import {
  openNodeInExplorer,
  openTopFileInExplorer,
  scanChildren,
  scanSummary,
  scanTopFiles,
} from "../api";
import { IconFolder, IconSpace } from "../components/Icons";
import { Treemap } from "../components/Treemap";
import { AlertBanner, EmptyState, StatusBadge } from "../components/Ui";
import { formatSize, formatTime } from "../format";
import type { DirNode, FileEntry, ScanSummary } from "../types";

const TOP_FILE_LIMIT = 200;

type Tab = "dirs" | "graph" | "files";

export function SpacePage({ onMigrate }: { onMigrate: (nodeId: number) => void }) {
  const [summary, setSummary] = useState<ScanSummary | null>(null);
  const [tab, setTab] = useState<Tab>("dirs");
  // 面包屑：从根目录逐级进入的节点，空数组表示还停在根层。
  const [trail, setTrail] = useState<DirNode[]>([]);
  const [rows, setRows] = useState<DirNode[]>([]);
  const [files, setFiles] = useState<FileEntry[]>([]);
  const [error, setError] = useState<string | null>(null);

  // 传 null 表示要根节点。
  const currentId = trail.length > 0 ? trail[trail.length - 1].id : null;

  useEffect(() => {
    scanSummary()
      .then(setSummary)
      .catch(() => setSummary(null));
  }, []);

  useEffect(() => {
    // 目录列表和图形视图看的都是当前节点的直接子目录，共用同一份数据。
    if (!summary || tab === "files") {
      return;
    }
    scanChildren(currentId)
      .then(setRows)
      .catch((reason) => setError(String(reason)));
  }, [summary, tab, currentId]);

  useEffect(() => {
    if (!summary || tab !== "files") {
      return;
    }
    scanTopFiles(TOP_FILE_LIMIT)
      .then(setFiles)
      .catch((reason) => setError(String(reason)));
  }, [summary, tab]);

  if (!summary) {
    return (
      <section className="page-section">
        <div className="page-header">
          <div>
            <h2 className="page-title">空间分析</h2>
            <p className="page-subtitle">逐级浏览目录占用与大文件分布</p>
          </div>
        </div>
        <div className="card">
          <EmptyState
            icon={<IconSpace />}
            title="还没有扫描结果"
            description="还没有扫描结果。请先到“总览”页扫描 C 盘或用户目录。"
          />
        </div>
      </section>
    );
  }

  return (
    <section className="page-section">
      <div className="page-header">
        <div>
          <h2 className="page-title">空间分析</h2>
          <p className="page-subtitle td-num">
            扫描起点 {summary.root} · 占盘 {formatSize(summary.allocated)} · {summary.files.toLocaleString()} 个文件
          </p>
        </div>

        <div className="tabs" role="tablist" aria-label="空间分析视图">
          <button
            type="button"
            role="tab"
            aria-selected={tab === "dirs"}
            className={tab === "dirs" ? "tab tab-active" : "tab"}
            onClick={() => setTab("dirs")}
          >
            目录
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={tab === "graph"}
            className={tab === "graph" ? "tab tab-active" : "tab"}
            onClick={() => setTab("graph")}
          >
            图形
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={tab === "files"}
            className={tab === "files" ? "tab tab-active" : "tab"}
            onClick={() => setTab("files")}
          >
            Top 文件
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

      {error && <AlertBanner tone="danger">{error}</AlertBanner>}

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
                  <th>名称</th>
                  <th>占盘</th>
                  <th>逻辑大小</th>
                  <th>文件数</th>
                  <th>占父目录</th>
                  <th>标记</th>
                  <th>操作</th>
                </tr>
              </thead>
              <tbody>
                {rows.length === 0 && (
                  <tr>
                    <td colSpan={7} className="muted">
                      这个目录下没有可统计的子目录
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
                          onClick={() => setTrail([...trail, node])}
                        >
                          <IconFolder width={14} height={14} />
                          <span>{node.name}</span>
                        </button>
                      ) : (
                        <span>{node.name}</span>
                      )}
                    </td>
                    <td className="td-num">{formatSize(node.allocated)}</td>
                    <td className="td-num">{formatSize(node.logical)}</td>
                    <td className="td-num">{node.files.toLocaleString()}</td>
                    <td>
                      <div className="bar bar-thin" title={`${(node.percentOfParent * 100).toFixed(1)}%`}>
                        <div
                          className="bar-fill"
                          style={{ width: `${Math.min(node.percentOfParent, 1) * 100}%` }}
                        />
                      </div>
                    </td>
                    <td>{renderMarks(node)}</td>
                    <td>
                      <div className="actions">
                        <button
                          type="button"
                          className="btn-link"
                          onClick={() =>
                            openNodeInExplorer(node.id).catch((reason) => setError(String(reason)))
                          }
                        >
                          打开位置
                        </button>
                        <button
                          type="button"
                          className="btn-link"
                          disabled={!node.hasChildren}
                          onClick={() => onMigrate(node.id)}
                        >
                          迁移到其他盘…
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
          <div className="card-header">
            <h3 className="card-title">最大文件 Top {TOP_FILE_LIMIT}</h3>
            <span className="muted">只读展示，可通过「打开位置」手动处理</span>
          </div>
          <div className="table-wrap">
            <table className="table">
              <thead>
                <tr>
                  <th>#</th>
                  <th>路径</th>
                  <th>占盘</th>
                  <th>逻辑大小</th>
                  <th>修改时间</th>
                  <th>操作</th>
                </tr>
              </thead>
              <tbody>
                {files.length === 0 && (
                  <tr>
                    <td colSpan={6} className="muted">
                      没有统计到文件
                    </td>
                  </tr>
                )}
                {files.map((file) => (
                  <tr key={file.index}>
                    <td className="td-num">{file.index + 1}</td>
                    <td className="path-cell" title={file.path}>
                      {file.path}
                    </td>
                    <td className="td-num">{formatSize(file.allocated)}</td>
                    <td className="td-num">{formatSize(file.logical)}</td>
                    <td className="td-num">{formatTime(file.lastWrite)}</td>
                    <td>
                      <button
                        type="button"
                        className="btn-link"
                        onClick={() =>
                          openTopFileInExplorer(file.index).catch((reason) => setError(String(reason)))
                        }
                      >
                        打开位置
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </section>
  );
}

function renderMarks(node: DirNode) {
  if (!node.denied && node.reparseSkipped === 0) {
    return <span className="muted">—</span>;
  }

  return (
    <div className="actions">
      {node.denied && <StatusBadge tone="caution">无权读取</StatusBadge>}
      {node.reparseSkipped > 0 && <StatusBadge tone="info">已跳过链接</StatusBadge>}
    </div>
  );
}
