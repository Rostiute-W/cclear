import { useEffect, useState } from "react";
import { appInfo } from "./api";
import type { AppInfo } from "./types";
import { CleanPage } from "./pages/Clean";
import { HistoryPage } from "./pages/History";
import { MigratePage } from "./pages/Migrate";
import { OverviewPage } from "./pages/Overview";
import { QuarantinePage } from "./pages/Quarantine";
import { SpacePage } from "./pages/Space";

const NAV = [
  { id: "overview", label: "总览" },
  { id: "clean", label: "清理" },
  { id: "space", label: "空间分析" },
  { id: "quarantine", label: "隔离区" },
  { id: "migrate", label: "迁移" },
  { id: "history", label: "历史" },
] as const;

type PageId = (typeof NAV)[number]["id"];

function renderPage(page: PageId) {
  switch (page) {
    case "overview":
      return <OverviewPage />;
    case "clean":
      return <CleanPage />;
    case "space":
      return <SpacePage />;
    case "quarantine":
      return <QuarantinePage />;
    case "migrate":
      return <MigratePage />;
    case "history":
      return <HistoryPage />;
  }
}

function App() {
  const [page, setPage] = useState<PageId>("overview");
  const [info, setInfo] = useState<AppInfo | null>(null);

  useEffect(() => {
    // 权限徽标只用于展示，取不到时保持“普通权限”。
    appInfo()
      .then(setInfo)
      .catch(() => setInfo(null));
  }, []);

  return (
    <div className="app">
      <header className="topbar">
        <span className="brand">CClear</span>
        <span className="version">版本 {info?.version ?? "—"}</span>
        <span className={info?.isAdmin ? "badge badge-admin" : "badge"}>
          {info?.isAdmin ? "管理员" : "普通权限"}
        </span>
      </header>

      <div className="layout">
        <nav className="sidebar">
          {NAV.map((item) => (
            <button
              key={item.id}
              type="button"
              className={item.id === page ? "nav-item nav-item-active" : "nav-item"}
              onClick={() => setPage(item.id)}
            >
              {item.label}
            </button>
          ))}
        </nav>

        <main className="content">{renderPage(page)}</main>
      </div>
    </div>
  );
}

export default App;
