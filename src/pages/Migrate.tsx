// 迁移页：把开发工具的缓存目录按官方方式改到其他盘，并按冻结清单清理旧目录。
import { useEffect, useState } from "react";
import {
  migrationCleanupOld,
  migrationInspect,
  migrationRevert,
  migrationStart,
  migrationsList,
} from "../api";
import { PlanPreview } from "../components/PlanPreview";
import { formatSize } from "../format";
import type { MigrationMode, MigrationToolState, MigrationsState, PlanSummary } from "../types";

// 清理旧目录用的临时规则不在内置规则目录里，预览里给它一个中文名。
const OLD_CACHE_RULE_NAME = "旧缓存目录";

// 迁移过的工具才谈得上撤销。
const REVERTIBLE_STAGES = ["verified", "cleanupPlanned", "cleaned"];

export function MigratePage() {
  const [state, setState] = useState<MigrationsState | null>(null);
  const [volume, setVolume] = useState("");
  const [mode, setMode] = useState<MigrationMode>("migrate");
  const [busy, setBusy] = useState(false);
  // 清理预览正在执行：它和迁移操作共用执行锁，期间迁移按钮必须一起禁用。
  const [planRunning, setPlanRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  // 一次只展开一份清理清单：同一时间只允许一次清理类操作。
  const [plan, setPlan] = useState<PlanSummary | null>(null);

  const reload = () => {
    migrationsList()
      .then((next) => {
        setState(next);
        // 选中的卷可能已经不在了，退回第一个候选卷。
        setVolume((current) =>
          next.volumes.some((item) => item.root === current)
            ? current
            : (next.volumes[0]?.root ?? ""),
        );
      })
      .catch((reason) => setError(String(reason)));
  };

  useEffect(reload, []);

  const run = async (action: () => Promise<string>) => {
    setBusy(true);
    setError(null);
    setMessage(null);

    try {
      setMessage(await action());
      reload();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const onStart = (tool: MigrationToolState) => {
    if (!volume) {
      setError("请先选择目标盘");
      return;
    }

    void run(async () => {
      // 进程还在写缓存时复制会搬到一半，确认之后才继续。
      const fresh = await migrationInspect(tool.tool);
      if (fresh.processes.length > 0) {
        const confirmed = window.confirm(
          `${fresh.label} 的相关进程正在运行：${fresh.processes.join("、")}。` +
            "迁移期间缓存可能被写入，建议先退出这些程序。仍要继续吗？",
        );
        if (!confirmed) {
          return "已取消迁移";
        }
      }

      const result = await migrationStart(tool.tool, volume, mode);
      return `${result.label}：${result.note}`;
    });
  };

  const onCleanup = (tool: MigrationToolState) => {
    void run(async () => {
      const created = await migrationCleanupOld(tool.tool);
      setPlan(created.summary);
      return `${tool.label}：已生成旧目录清理清单，请在下方的预览里确认后再执行`;
    });
  };

  const onRevert = (tool: MigrationToolState) => {
    if (!window.confirm(`撤销 ${tool.label} 的迁移？缓存位置会回到迁移前的配置。`)) {
      return;
    }

    void run(async () => {
      const result = await migrationRevert(tool.tool);
      setPlan(null);
      return `${result.label}：${result.note}`;
    });
  };

  return (
    <section>
      <h2>迁移</h2>

      {error && <p className="warn">{error}</p>}
      {message && <p className="muted">{message}</p>}

      <div className="card">
        <h3>目标盘与方式</h3>

        {!state && <p className="muted">正在读取工具缓存位置…</p>}

        {state && (
          <>
            <div className="volume-row">
              <span className="volume-name">目标盘</span>
              <select
                value={volume}
                disabled={busy || state.volumes.length === 0}
                onChange={(event) => setVolume(event.target.value)}
              >
                {state.volumes.map((item) => (
                  <option key={item.root} value={item.root}>
                    {item.root} {item.label}（剩余 {formatSize(item.free)}）
                  </option>
                ))}
              </select>
            </div>

            <div className="volume-row">
              <span className="volume-name">迁移方式</span>
              <select
                value={mode}
                disabled={busy}
                onChange={(event) => setMode(event.target.value as MigrationMode)}
              >
                <option value="migrate">迁移旧缓存：先复制并校验，再改配置</option>
                <option value="rebuild">允许重建：不复制，工具在新位置重新下载</option>
              </select>
            </div>

            {state.volumes.length === 0 && (
              <p className="warn">
                没有可用的目标盘：缓存目录只能放在非系统盘的本地 NTFS 固定盘上。
              </p>
            )}
            {state.volumes.length > 0 && (
              <p className="muted">
                缓存会放在 {volume}CClearData\ 下面，各工具一个子目录。
              </p>
            )}
          </>
        )}
      </div>

      <div className="card">
        <h3>工具缓存目录</h3>

        {state && (
          <table className="table">
            <thead>
              <tr>
                <th>工具</th>
                <th>当前路径</th>
                <th>大小</th>
                <th>在 C 盘</th>
                <th>状态</th>
                <th>操作</th>
              </tr>
            </thead>
            <tbody>
              {state.tools.map((tool) => (
                <tr key={tool.tool}>
                  <td>{tool.label}</td>
                  <td>
                    {tool.entries.map((entry) => (
                      <div key={entry.name}>
                        <span className="muted">{entry.name}：</span>
                        <span className="path-text" title={entry.path}>
                          {entry.path}
                        </span>
                        <span className="muted">
                          （{entry.source}
                          {entry.exists ? "" : "，目录不存在"}）
                        </span>
                      </div>
                    ))}
                    {tool.warning && <div className="warn">{tool.warning}</div>}
                    {tool.needsProgram && !tool.installed && (
                      <div className="warn">没有在 PATH 里找到 {tool.label}，无法验证新位置。</div>
                    )}
                  </td>
                  <td>{formatSize(tool.totalSize)}</td>
                  <td>{tool.onSystemDrive ? "是" : "否"}</td>
                  <td>
                    {tool.stageLabel ? (
                      <span className="status-badge">{tool.stageLabel}</span>
                    ) : (
                      <span className="muted">还没迁移</span>
                    )}
                    {tool.note && <div className="muted">{tool.note}</div>}
                    {tool.target && <div className="muted">目标：{tool.target}</div>}
                  </td>
                  <td>
                    <div className="actions">
                      <button
                        type="button"
                        disabled={
                          busy ||
                          planRunning ||
                          !volume ||
                          (tool.needsProgram && !tool.installed)
                        }
                        onClick={() => onStart(tool)}
                      >
                        开始迁移
                      </button>
                      <button
                        type="button"
                        disabled={busy || planRunning || tool.oldDirs.length === 0}
                        onClick={() => onCleanup(tool)}
                      >
                        清理旧目录
                      </button>
                      <button
                        type="button"
                        disabled={
                          busy ||
                          planRunning ||
                          !REVERTIBLE_STAGES.includes(tool.stage ?? "")
                        }
                        onClick={() => onRevert(tool)}
                      >
                        撤销
                      </button>
                    </div>
                    {tool.oldDirs.length > 0 && (
                      <div className="muted">旧目录：{tool.oldDirs.join("、")}</div>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}

        <p className="muted">
          CARGO_HOME 和 RUSTUP_HOME 不提供自动迁移：请先手工把旧内容移到新位置，再改这两个环境变量，
          否则 Cargo 会按新位置重新下载全部依赖。
        </p>
        <p className="muted">
          迁移完成后请重启 IDE 和终端：已经在运行的程序不会读取新的环境变量。
        </p>
      </div>

      {plan && (
        <PlanPreview
          planId={plan.planId}
          summary={plan}
          ruleNames={Object.fromEntries(
            plan.rules.map((rule) => [rule.ruleId, OLD_CACHE_RULE_NAME]),
          )}
          onFinished={reload}
          onRunningChange={setPlanRunning}
          blocked={busy}
        />
      )}
    </section>
  );
}
