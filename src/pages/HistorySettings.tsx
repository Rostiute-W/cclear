import { Fragment, useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import {
  historyDetail,
  historyList,
  rulePackImport,
  rulePackRemove,
  settingsGet,
  settingsSave,
} from "../api";
import { IconHistory, IconShield, IconTrash, IconUpload } from "../components/Icons";
import { AlertBanner, EmptyState, StatusBadge } from "../components/Ui";
import { formatDuration, formatSize, formatTime } from "../format";
import type { AppSettings, CleanHistoryDetail, CleanHistoryList, RulePackInfo } from "../types";

type SubTab = "history" | "rules" | "prefs";

export function HistorySettingsPage({
  refreshTick,
  onRulePacksChanged,
}: {
  refreshTick: number;
  onRulePacksChanged: () => void;
}) {
  const [subTab, setSubTab] = useState<SubTab>("history");

  // 历史记录状态
  const [days, setDays] = useState(30);
  const [history, setHistory] = useState<CleanHistoryList | null>(null);
  const [expandedRun, setExpandedRun] = useState<string | null>(null);
  const [runDetail, setRunDetail] = useState<CleanHistoryDetail | null>(null);

  // 设置与规则库状态
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [whitelistInput, setWhitelistInput] = useState("");
  const [saving, setSaving] = useState(false);
  const [importing, setImporting] = useState(false);
  const [showSchemaHint, setShowSchemaHint] = useState(false);
  const [lastImportedPack, setLastImportedPack] = useState<RulePackInfo | null>(null);
  const [expandedPackId, setExpandedPackId] = useState<string | null>(null);

  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const loadHistory = useCallback(() => {
    historyList(days)
      .then(setHistory)
      .catch((reason) => setError(String(reason)));
  }, [days]);

  const loadSettings = useCallback(() => {
    settingsGet()
      .then(setSettings)
      .catch((reason) => setError(String(reason)));
  }, []);

  useEffect(() => {
    loadHistory();
    loadSettings();
  }, [loadHistory, loadSettings, refreshTick]);

  const toggleRunDetail = (runId: string) => {
    if (expandedRun === runId) {
      setExpandedRun(null);
      setRunDetail(null);
      return;
    }
    setExpandedRun(runId);
    setRunDetail(null);
    historyDetail(runId, days)
      .then(setRunDetail)
      .catch((reason) => setError(String(reason)));
  };

  const handleAddWhitelist = async (event: FormEvent) => {
    event.preventDefault();
    const trimmed = whitelistInput.trim();
    if (!trimmed || !settings) return;
    setError(null);
    setNotice(null);
    setSaving(true);
    try {
      const updated = await settingsSave({
        whitelistPaths: [...settings.whitelistPaths, trimmed],
      });
      setSettings(updated);
      setWhitelistInput("");
      setNotice(`已将「${trimmed}」加入保护白名单`);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setSaving(false);
    }
  };

  const handleRemoveWhitelist = async (target: string) => {
    if (!settings) return;
    setError(null);
    setNotice(null);
    setSaving(true);
    try {
      const updated = await settingsSave({
        whitelistPaths: settings.whitelistPaths.filter((item) => item !== target),
      });
      setSettings(updated);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setSaving(false);
    }
  };

  const handlePrefUpdate = async (patch: {
    scheduleRebootDeleteDefault?: boolean;
    minTempAgeHours?: number;
  }) => {
    setError(null);
    setNotice(null);
    setSaving(true);
    try {
      const updated = await settingsSave(patch);
      setSettings(updated);
      setNotice("清理偏好已保存");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setSaving(false);
    }
  };

  const handleImportRulePack = async (file: File | null) => {
    if (!file) return;
    setError(null);
    setNotice(null);
    setImporting(true);
    try {
      const text = await file.text();
      const pack = await rulePackImport(text);
      setLastImportedPack(pack);
      setExpandedPackId(pack.id);
      setNotice(`已导入规则包「${pack.name}」v${pack.version}（${pack.ruleCount} 条规则），请核对下方规则目标与删除方式`);
      loadSettings();
      onRulePacksChanged();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setImporting(false);
      if (fileInputRef.current) {
        fileInputRef.current.value = "";
      }
    }
  };

  const handleRemoveRulePack = async (packId: string, name: string) => {
    if (!window.confirm(`确定移除外置规则包「${name}」吗？`)) return;
    setError(null);
    setNotice(null);
    try {
      await rulePackRemove(packId);
      if (lastImportedPack?.id === packId) setLastImportedPack(null);
      if (expandedPackId === packId) setExpandedPackId(null);
      setNotice(`已移除规则包「${name}」`);
      loadSettings();
      onRulePacksChanged();
    } catch (reason) {
      setError(String(reason));
    }
  };

  return (
    <section className="page-section">
      <div className="page-header">
        <div>
          <h2 className="page-title">历史与设置</h2>
          <p className="page-subtitle">查看每次清理释放记录，管理保护白名单与本地规则包</p>
        </div>

        <div className="tabs" role="tablist" aria-label="设置分区">
          <button
            type="button"
            role="tab"
            aria-selected={subTab === "history"}
            className={subTab === "history" ? "tab tab-active" : "tab"}
            onClick={() => setSubTab("history")}
          >
            清理历史
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={subTab === "rules"}
            className={subTab === "rules" ? "tab tab-active" : "tab"}
            onClick={() => setSubTab("rules")}
          >
            规则库与白名单
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={subTab === "prefs"}
            className={subTab === "prefs" ? "tab tab-active" : "tab"}
            onClick={() => setSubTab("prefs")}
          >
            清理偏好
          </button>
        </div>
      </div>

      {error && <AlertBanner tone="danger">{error}</AlertBanner>}
      {notice && <AlertBanner tone="success">{notice}</AlertBanner>}

      {/* 子页 1：清理历史 */}
      {subTab === "history" && (
        <div className="card">
          <div className="card-header">
            <h3 className="card-title">最近清理记录</h3>
            <div className="actions">
              {[7, 30, 90].map((d) => (
                <button
                  key={d}
                  type="button"
                  className={days === d ? "chip chip-active" : "chip"}
                  onClick={() => setDays(d)}
                >
                  近 {d} 天
                </button>
              ))}
            </div>
          </div>

          {!history || history.runs.length === 0 ? (
            <EmptyState
              icon={<IconHistory width={34} height={34} />}
              title="暂无清理记录"
              description="在「一键清理」页面完成清理后，释放体积与明细会自动记录在这里。"
            />
          ) : (
            <div className="table-wrap">
              <table className="table">
                <thead>
                  <tr>
                    <th>时间</th>
                    <th>释放空间</th>
                    <th>删除 / 回收站 / 跳过</th>
                    <th>耗时</th>
                    <th>状态</th>
                    <th>明细</th>
                  </tr>
                </thead>
                <tbody>
                  {history.runs.map((run) => {
                    const isOpen = expandedRun === run.runId;
                    const actual = Math.max(0, run.freeAfter - run.freeBefore);
                    return (
                      <Fragment key={run.runId}>
                        <tr>
                          <td className="td-num td-nowrap">{formatTime(run.startedAt)}</td>
                          <td className="td-num">
                            <strong>{formatSize(run.freedBytes)}</strong>
                            {actual > 0 && (
                              <span className="muted">（实增 {formatSize(actual)}）</span>
                            )}
                          </td>
                          <td className="td-num">
                            {run.deletedFiles} 删 / {run.recycledItems} 回收站 / {run.skippedFiles}{" "}
                            跳过
                            {run.rebootScheduled > 0 && ` / ${run.rebootScheduled} 重启删`}
                          </td>
                          <td className="td-num">{formatDuration(run.elapsedMs)}</td>
                          <td>
                            <StatusBadge tone={run.cancelled ? "caution" : "safe"}>
                              {run.cancelled ? "已停止" : "完成"}
                            </StatusBadge>
                          </td>
                          <td>
                            <button
                              type="button"
                              className="btn-link"
                              onClick={() => toggleRunDetail(run.runId)}
                            >
                              {isOpen ? "收起" : "展开"}
                            </button>
                          </td>
                        </tr>
                        {isOpen && (
                          <tr className="sub-row">
                            <td colSpan={6}>
                              {!runDetail ? (
                                <span className="muted">正在加载明细…</span>
                              ) : (
                                <ul className="detail-list">
                                  {runDetail.items.map((item) => (
                                    <li key={item.itemId}>
                                      <strong>{item.name}</strong> ·{" "}
                                      <span className="td-num">{formatSize(item.freedBytes)}</span>{" "}
                                      · <span className="muted">{item.detail}</span>
                                    </li>
                                  ))}
                                </ul>
                              )}
                            </td>
                          </tr>
                        )}
                      </Fragment>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </div>
      )}

      {/* 子页 2：规则库与白名单 */}
      {subTab === "rules" && (
        <div className="settings-stack">
          <div className="card">
            <div className="card-header">
              <div>
                <h3 className="card-title">规则库管理</h3>
                <p className="muted" style={{ margin: "2px 0 0" }}>
                  采用声明式 JSON 规则包（仿火绒规则库机制），外置规则包自动经过安全禁区与路径校验，禁止执行任意命令。
                </p>
              </div>
              <div className="actions">
                <input
                  ref={fileInputRef}
                  type="file"
                  accept=".json"
                  style={{ display: "none" }}
                  onChange={(event) =>
                    handleImportRulePack(event.target.files?.[0] ?? null)
                  }
                />
                <button
                  type="button"
                  className="btn btn-subtle"
                  onClick={() => setShowSchemaHint((prev) => !prev)}
                >
                  {showSchemaHint ? "收起格式示例" : "规则包格式示例"}
                </button>
                <button
                  type="button"
                  className="btn btn-primary"
                  disabled={importing}
                  onClick={() => fileInputRef.current?.click()}
                >
                  <IconUpload width={14} height={14} />
                  <span>{importing ? "校验导入中…" : "导入 JSON 规则包"}</span>
                </button>
              </div>
            </div>

            {showSchemaHint && (
              <pre className="schema-pre">
{`{
  "id": "my-custom-pack",
  "name": "个人扩展规则包",
  "version": "1.0.0",
  "description": "补充常用软件缓存路径",
  "rules": [
    {
      "id": "custom-app-cache",
      "name": "示例应用缓存",
      "description": "可自动重建的缓存文件",
      "category": "app",
      "defaultSelected": true,
      "admin": false,
      "deleteMode": "direct",
      "processes": ["ExampleApp.exe"],
      "target": {
        "kind": "paths",
        "paths": [
          {
            "root": "localAppData",
            "rels": ["ExampleApp\\\\Cache"]
          }
        ]
      }
    }
  ]
}`}
              </pre>
            )}

            {lastImportedPack && (
              <div className="sample-box" style={{ marginBottom: 12, borderRadius: "var(--radius-md)" }}>
                <div className="sample-title">
                  刚导入的规则明细：{lastImportedPack.name} v{lastImportedPack.version}（共{" "}
                  {lastImportedPack.rulesSummary.length} 条）
                </div>
                <ul className="detail-list">
                  {lastImportedPack.rulesSummary.map((rule) => (
                    <li key={rule.id}>
                      <strong>{rule.name}</strong>（
                      {rule.deleteMode === "recycleBin" ? "移入回收站" : "直接删除"}）·{" "}
                      <span className="path-cell">{rule.targets.join("；")}</span>
                    </li>
                  ))}
                </ul>
              </div>
            )}

            <div className="table-wrap">
              <table className="table">
                <thead>
                  <tr>
                    <th>规则包名称</th>
                    <th>版本</th>
                    <th>规则数</th>
                    <th>来源</th>
                    <th>说明</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  {(settings?.rulePacks ?? []).map((pack) => {
                    const isPackOpen = expandedPackId === pack.id;
                    return (
                      <Fragment key={pack.id}>
                        <tr>
                          <td className="td-nowrap">
                            <strong>{pack.name}</strong>
                          </td>
                          <td className="td-num">{pack.version}</td>
                          <td className="td-num">{pack.ruleCount}</td>
                          <td>
                            <StatusBadge tone={pack.builtin ? "safe" : "info"}>
                              {pack.builtin ? "内置官方" : "本地扩展"}
                            </StatusBadge>
                          </td>
                          <td>{pack.description}</td>
                          <td className="td-nowrap">
                            <div className="actions">
                              {pack.rulesSummary.length > 0 && (
                                <button
                                  type="button"
                                  className="btn-link"
                                  onClick={() =>
                                    setExpandedPackId(isPackOpen ? null : pack.id)
                                  }
                                >
                                  {isPackOpen ? "收起规则" : "查看规则"}
                                </button>
                              )}
                              {!pack.builtin && (
                                <button
                                  type="button"
                                  className="btn-link danger-link"
                                  onClick={() => handleRemoveRulePack(pack.id, pack.name)}
                                >
                                  <IconTrash width={13} height={13} />
                                  <span>移除</span>
                                </button>
                              )}
                            </div>
                          </td>
                        </tr>
                        {isPackOpen && pack.rulesSummary.length > 0 && (
                          <tr className="sub-row">
                            <td colSpan={6}>
                              <ul className="detail-list">
                                {pack.rulesSummary.map((rule) => (
                                  <li key={rule.id}>
                                    <strong>{rule.name}</strong>（
                                    {rule.deleteMode === "recycleBin"
                                      ? "移入回收站"
                                      : rule.deleteMode === "command"
                                        ? "官方命令"
                                        : rule.deleteMode === "systemOp"
                                          ? "系统维护"
                                          : "直接删除"}
                                    ）·{" "}
                                    <span className="path-cell">{rule.targets.join("；")}</span>
                                  </li>
                                ))}
                              </ul>
                            </td>
                          </tr>
                        )}
                      </Fragment>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </div>

          <div className="card">
            <div className="card-header">
              <div>
                <h3 className="card-title">
                  <IconShield width={15} height={15} /> 保护白名单（排除目录）
                </h3>
                <p className="muted" style={{ margin: "2px 0 0" }}>
                  加入白名单的目录及其子目录在所有规则与启发式扫描中均会被永久跳过。
                </p>
              </div>
            </div>

            <form className="whitelist-form" onSubmit={handleAddWhitelist}>
              <input
                type="text"
                className="text-input"
                value={whitelistInput}
                disabled={saving}
                placeholder="输入完整本地目录路径，例如 C:\Users\你的名字\AppData\Local\MyImportantApp"
                onChange={(event) => setWhitelistInput(event.target.value)}
              />
              <button type="submit" className="btn btn-primary" disabled={saving || !whitelistInput.trim()}>
                添加保护
              </button>
            </form>

            {settings && settings.whitelistPaths.length > 0 ? (
              <ul className="whitelist-list">
                {settings.whitelistPaths.map((path) => (
                  <li key={path} className="whitelist-row">
                    <span className="path-cell">{path}</span>
                    <button
                      type="button"
                      className="btn-link danger-link"
                      disabled={saving}
                      onClick={() => handleRemoveWhitelist(path)}
                    >
                      移除
                    </button>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="muted" style={{ margin: "12px 0 0" }}>
                当前未设置自定义白名单（系统核心目录默认始终受到底层禁区保护）。
              </p>
            )}
          </div>
        </div>
      )}

      {/* 子页 3：清理偏好 */}
      {subTab === "prefs" && settings && (
        <div className="card prefs-card">
          <h3 className="card-title" style={{ marginBottom: 14 }}>
            清理策略偏好
          </h3>

          <div className="pref-row">
            <div>
              <strong>临时文件最小保留时长</strong>
              <p className="muted">
                防止误删刚刚由安装程序或运行中应用生成的临时文件，只清理超过此时长的文件。
              </p>
            </div>
            <select
              className="select-input"
              value={settings.minTempAgeHours}
              disabled={saving}
              onChange={(event) =>
                handlePrefUpdate({ minTempAgeHours: Number(event.target.value) })
              }
            >
              <option value={24}>24 小时（推荐）</option>
              <option value={72}>3 天（72 小时）</option>
              <option value={168}>7 天（168 小时）</option>
            </select>
          </div>

          <div className="pref-row">
            <div>
              <strong>顽固系统垃圾默认排入“重启后删除”</strong>
              <p className="muted">
                遇到被系统服务锁定的 Windows 临时/更新缓存文件时，自动通过系统队列安排下次开机前清理。
              </p>
            </div>
            <label className="inline-check">
              <input
                type="checkbox"
                checked={settings.scheduleRebootDeleteDefault}
                disabled={saving}
                onChange={(event) =>
                  handlePrefUpdate({ scheduleRebootDeleteDefault: event.target.checked })
                }
              />
              <span>默认启用</span>
            </label>
          </div>
        </div>
      )}
    </section>
  );
}
