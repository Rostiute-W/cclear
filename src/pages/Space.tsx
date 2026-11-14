import { useEffect, useState } from "react";
import {
  openNodeInExplorer,
  openTopFileInExplorer,
  scanChildren,
  scanSummary,
  scanTopFiles,
} from "../api";
import { Treemap } from "../components/Treemap";
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
      <section>
        <h2>空间分析</h2>
        <p className="placeholder">还没有扫描结果。请先到“总览”页扫描 C 盘或用户目录。</p>
      </section>
    );
  }

  return (
    <section>
      <h2>空间分析</h2>

      <nav className="crumbs">
        <button type="button" className="crumb" onClick={() => setTrail([])}>
          {summary.root}
        </button>
        {trail.map((node, index) => (
          <span key={node.id}>
            <span className="crumb-sep">›</span>
            <button
              type="button"
              className="crumb"
              onClick={() => setTrail(trail.slice(0, index + 1))}
            >
              {node.name}
            </button>
          </span>
        ))}
      </nav>

      <div className="tabs">
        <button
          type="button"
          className={tab === "dirs" ? "tab tab-active" : "tab"}
          onClick={() => setTab("dirs")}
        >
          目录
        </button>
        <button
          type="button"
          className={tab === "graph" ? "tab tab-active" : "tab"}
          onClick={() => setTab("graph")}
        >
          图形
        </button>
        <button
          type="button"
          className={tab === "files" ? "tab tab-active" : "tab"}
          onClick={() => setTab("files")}
        >
          Top 文件
        </button>
      </div>

      {error && <p className="warn">{error}</p>}

      {tab === "graph" && (
        <Treemap nodes={rows} onEnter={(node) => setTrail([...trail, node])} />
      )}

      {tab === "dirs" && (
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
                      className="link"
                      onClick={() => setTrail([...trail, node])}
                    >
                      {node.name}
                    </button>
                  ) : (
                    node.name
                  )}
                </td>
                <td>{formatSize(node.allocated)}</td>
                <td>{formatSize(node.logical)}</td>
                <td>{node.files.toLocaleString()}</td>
                <td>
                  <div className="bar bar-thin">
                    <div
                      className="bar-fill"
                      style={{ width: `${Math.min(node.percentOfParent, 1) * 100}%` }}
                    />
                  </div>
                </td>
                <td>{marks(node)}</td>
                <td>
                  <button
                    type="button"
                    className="link"
                    onClick={() =>
                      openNodeInExplorer(node.id).catch((reason) => setError(String(reason)))
                    }
                  >
                    打开位置
                  </button>
                  <button
                    type="button"
                    className="link"
                    disabled={!node.hasChildren}
                    onClick={() => onMigrate(node.id)}
                  >
                    迁移到其他盘…
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {tab === "files" && (
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
                <td>{file.index + 1}</td>
                <td className="path-cell" title={file.path}>
                  {file.path}
                </td>
                <td>{formatSize(file.allocated)}</td>
                <td>{formatSize(file.logical)}</td>
                <td>{formatTime(file.lastWrite)}</td>
                <td>
                  <button
                    type="button"
                    className="link"
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
      )}
    </section>
  );
}

function marks(node: DirNode): string {
  const labels: string[] = [];
  if (node.denied) {
    labels.push("无权读取");
  }
  if (node.reparseSkipped > 0) {
    labels.push("已跳过链接");
  }
  return labels.length > 0 ? labels.join(" / ") : "—";
}
