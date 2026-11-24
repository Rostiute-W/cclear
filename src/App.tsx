import { useCallback, useEffect, useState } from "react";
import type { ReactNode } from "react";
import { appInfo } from "./api";
import { BrandMark, IconClean, IconHistory, IconSpace } from "./components/Icons";
import { StatusBadge } from "./components/Ui";
import { CleanPage } from "./pages/Clean";
import { HistorySettingsPage } from "./pages/HistorySettings";
import { SpacePage } from "./pages/Space";
import type { AppInfo } from "./types";

type PageId = "clean" | "space" | "settings";

const NAV: { id: PageId; label: string; desc: string; icon: ReactNode }[] = [
  { id: "clean", label: "一键清理", desc: "垃圾扫描与系统瘦身", icon: <IconClean /> },
  { id: "space", label: "空间分析", desc: "极速定位 C 盘大文件", icon: <IconSpace /> },
  { id: "settings", label: "历史与设置", desc: "记录、白名单与规则库", icon: <IconHistory /> },
];

export default function App() {
  const [page, setPage] = useState<PageId>("clean");
  const [app, setApp] = useState<AppInfo | null>(null);
  // 清理或导入规则包后通知相关面板刷新
  const [refreshTick, setRefreshTick] = useState(0);

  const loadAppInfo = useCallback(() => {
    appInfo()
      .then(setApp)
      .catch(() => setApp(null));
  }, []);

  useEffect(() => {
    loadAppInfo();
  }, [loadAppInfo, refreshTick]);

  const handleDataChanged = useCallback(() => {
    setRefreshTick((tick) => tick + 1);
  }, []);

  return (
    <div className="app-shell">
      <aside className="sidebar" aria-label="主导航">
        <div className="sidebar-brand">
          <BrandMark className="sidebar-brand-icon" />
          <div className="sidebar-brand-text">
            <span className="brand-name">CClear</span>
            <span className="brand-sub">C 盘极速空间清理</span>
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
              <span className="nav-text">
                <span className="nav-label">{item.label}</span>
                <span className="nav-desc">{item.desc}</span>
              </span>
            </button>
          ))}
        </nav>

        <div className="sidebar-footer">
          <div className="sidebar-meta-row">
            <span>版本</span>
            <span className="td-num">{app ? `v${app.version}` : "—"}</span>
          </div>
          <div className="sidebar-meta-row">
            <span>规则库</span>
            <span className="td-num">
              {app ? `${app.rulePackVersion} · ${app.ruleCount} 条` : "—"}
            </span>
          </div>
          <div className="sidebar-meta-row">
            <span>当前权限</span>
            <StatusBadge tone={app?.isAdmin ? "safe" : "neutral"}>
              {app?.isAdmin ? "管理员" : "普通权限"}
            </StatusBadge>
          </div>
        </div>
      </aside>

      <div className="main-stage">
        <main className="content">
          {/* 保留清理页与空间分析页的运行状态，切换页面不丢扫描结果 */}
          <div className={page === "clean" ? "page-pane" : "page-pane-hidden"}>
            <CleanPage onCleaned={handleDataChanged} onOpenSpace={() => setPage("space")} />
          </div>
          <div className={page === "space" ? "page-pane" : "page-pane-hidden"}>
            <SpacePage onRecycled={handleDataChanged} />
          </div>
          {page === "settings" && (
            <HistorySettingsPage
              refreshTick={refreshTick}
              onRulePacksChanged={handleDataChanged}
            />
          )}
        </main>
      </div>
    </div>
  );
}
