import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import { appInfo, quarantineOverdue } from "./api";
import {
  BrandMark,
  IconClean,
  IconHistory,
  IconMigrate,
  IconOverview,
  IconQuarantine,
  IconSpace,
} from "./components/Icons";
import { AlertBanner, StatusBadge } from "./components/Ui";
import { formatSize } from "./format";
import { CleanPage } from "./pages/Clean";
import { HistoryPage } from "./pages/History";
import { MigratePage } from "./pages/Migrate";
import { OverviewPage } from "./pages/Overview";
import { QuarantinePage } from "./pages/Quarantine";
import { SpacePage } from "./pages/Space";
import type { AppInfo, QuarantineBatch } from "./types";

type PageId = "overview" | "clean" | "space" | "quarantine" | "migrate" | "history";

const NAV: { id: PageId; label: string; icon: ReactNode }[] = [
  { id: "overview", label: "总览", icon: <IconOverview /> },
  { id: "clean", label: "清理", icon: <IconClean /> },
  { id: "space", label: "空间分析", icon: <IconSpace /> },
  { id: "quarantine", label: "隔离区", icon: <IconQuarantine /> },
  { id: "migrate", label: "迁移", icon: <IconMigrate /> },
  { id: "history", label: "历史", icon: <IconHistory /> },
];

export default function App() {
  const [page, setPage] = useState<PageId>("overview");
  const [app, setApp] = useState<AppInfo | null>(null);
  const [overdue, setOverdue] = useState<QuarantineBatch[]>([]);
  // 从空间分析页点“迁移到其他盘…”时，把选中目录的节点 ID 传给迁移页。
  const [migrateSource, setMigrateSource] = useState<number | null>(null);

  const refreshOverdue = () => {
    quarantineOverdue()
      .then(setOverdue)
      .catch(() => setOverdue([]));
  };

  useEffect(() => {
    appInfo()
      .then(setApp)
      .catch(() => setApp(null));
    refreshOverdue();
  }, []);

  const openMigrateForNode = (nodeId: number) => {
    setMigrateSource(nodeId);
    setPage("migrate");
  };

  return (
    <div className="app-shell">
      <aside className="sidebar" aria-label="主导航">
        <div className="sidebar-brand">
          <BrandMark className="sidebar-brand-icon" />
          <div className="sidebar-brand-text">
            <span className="brand-name">CClear</span>
            <span className="brand-sub">C 盘安全清理与迁移</span>
          </div>
        </div>

        <nav className="nav-list" aria-label="功能分区">
          {NAV.map((item) => (
            <button
              key={item.id}
              type="button"
              className={page === item.id ? "nav-item nav-item-active" : "nav-item"}
              aria-current={page === item.id ? "page" : undefined}
              onClick={() => setPage(item.id)}
            >
              <span className="nav-icon">{item.icon}</span>
              <span className="nav-label">{item.label}</span>
              {item.id === "quarantine" && overdue.length > 0 && (
                <span className="nav-pill" title={`有 ${overdue.length} 个超期批次`}>
                  {overdue.length}
                </span>
              )}
            </button>
          ))}
        </nav>

        <div className="sidebar-footer">
          <div className="sidebar-meta-row">
            <span>版本</span>
            <span className="td-num">{app ? `v${app.version}` : "—"}</span>
          </div>
          {/* 权限徽标只用于展示，取不到时保持“普通权限”。 */}
          <div className="sidebar-meta-row">
            <span>权限</span>
            <StatusBadge tone={app?.isAdmin ? "caution" : "neutral"}>
              {app?.isAdmin ? "管理员" : "普通权限"}
            </StatusBadge>
          </div>
        </div>
      </aside>

      <div className="main-stage">
        <main className="content">
          {overdue.length > 0 && page !== "quarantine" && (
            <AlertBanner
              tone="warn"
              actions={
                <button
                  type="button"
                  className="btn btn-subtle"
                  onClick={() => setPage("quarantine")}
                >
                  去隔离区处理
                </button>
              }
            >
              <span>
                隔离区有 {overdue.length} 个批次存放超过 7 天，共{" "}
                {formatSize(overdue.reduce((total, batch) => total + batch.size, 0))}，需要你手动处理。
              </span>
            </AlertBanner>
          )}

          {page === "overview" && <OverviewPage />}
          {page === "clean" && <CleanPage />}
          {page === "space" && <SpacePage onMigrate={openMigrateForNode} />}
          {page === "quarantine" && <QuarantinePage onChanged={refreshOverdue} />}
          {page === "migrate" && (
            <MigratePage
              initialSourceId={migrateSource}
              onSourceHandled={() => setMigrateSource(null)}
            />
          )}
          {page === "history" && <HistoryPage />}
        </main>
      </div>
    </div>
  );
}
