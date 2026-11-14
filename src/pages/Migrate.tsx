// 迁移页：把开发工具的缓存目录按官方方式改到其他盘，并按冻结清单清理旧目录；
// 也负责纯数据目录的 junction 迁移与迁回。
import { useEffect, useState } from "react";
import {
  junctionCheck,
  junctionContinue,
  junctionDeleteBackup,
  junctionDiscardTarget,
  junctionReturn,
  junctionStart,
  junctionState,
  migrationCleanupOld,
  migrationInspect,
  migrationRevert,
  migrationStart,
  migrationsList,
} from "../api";
import { PlanPreview } from "../components/PlanPreview";
import { formatSize } from "../format";
import type {
  DirMigration,
  DirMigrationStage,
  DirMigrationsState,
  JunctionCheck,
  MigrationMode,
  MigrationToolState,
  MigrationsState,
  PlanSummary,
} from "../types";

// 清理旧目录用的临时规则不在内置规则目录里，预览里给它一个中文名。
const OLD_CACHE_RULE_NAME = "旧缓存目录";

// 迁移过的工具才谈得上撤销。
const REVERTIBLE_STAGES = ["verified", "cleanupPlanned", "cleaned"];

// 目录迁移的步骤时间线，顺序和 Rust 端状态机一致。
const DIR_STEPS: readonly [DirMigrationStage, string][] = [
  ["precheck", "资格检查"],
  ["copying", "复制到目标卷"],
  ["verified", "复制校验"],
  ["sourceRenamed", "源目录改名成备份"],
  ["junctionCreated", "创建 junction"],
  ["validated", "经链接校验"],
  ["awaitingUserTest", "等待实测确认"],
  ["backupDeleted", "删除备份"],
  ["done", "完成"],
];

export function MigratePage({
  initialSourceId,
  onSourceHandled,
}: {
  // 空间分析页带过来的待迁移目录节点 ID。null 表示本次没有选目录。
  initialSourceId: number | null;
  onSourceHandled: () => void;
}) {
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
  // 目录迁移：记录和当前选中目录的资格检查结果。
  const [dirs, setDirs] = useState<DirMigrationsState | null>(null);
  const [dirCheck, setDirCheck] = useState<JunctionCheck | null>(null);

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

    junctionState()
      .then(setDirs)
      .catch((reason) => setError(String(reason)));
  };

  useEffect(reload, []);

  // 空间分析页选了目录之后，只拿节点 ID 去问后端能不能迁移。
  useEffect(() => {
    if (initialSourceId === null) {
      setDirCheck(null);
      return;
    }

    junctionCheck(initialSourceId)
      .then(setDirCheck)
      .catch((reason) => setError(String(reason)));
  }, [initialSourceId]);

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

  const onDirStart = (check: JunctionCheck) => {
    if (!volume) {
      setError("请先选择目标盘");
      return;
    }
    if (
      !window.confirm(
        `把 ${check.path} 迁移到 ${volume}？迁移期间请先退出正在使用这个目录的程序。` +
          "迁移完成之后请打开相关应用确认正常，再删除备份。",
      )
    ) {
      return;
    }

    void run(async () => {
      const record = await junctionStart(check.sourceId, volume);
      onSourceHandled();
      return `${record.source}：${record.note}`;
    });
  };

  const onDirContinue = (record: DirMigration) => {
    void run(async () => {
      const next = await junctionContinue(record.id);
      return `${next.source}：${next.note}`;
    });
  };

  const onDirReturn = (record: DirMigration) => {
    if (
      !window.confirm(
        `把 ${record.source} 迁回本机？迁回以目标里的最新数据为准，来源盘空间不足时会拒绝。`,
      )
    ) {
      return;
    }

    void run(async () => {
      const next = await junctionReturn(record.id);
      return `${next.source}：${next.note}`;
    });
  };

  const onDirDeleteBackup = (record: DirMigration) => {
    if (!window.confirm(`删除备份 ${record.backup}？删除之后不能再用它恢复这次迁移。`)) {
      return;
    }

    void run(async () => {
      const next = await junctionDeleteBackup(record.id);
      return `${next.source}：${next.note}`;
    });
  };

  const onDirDiscardTarget = (record: DirMigration) => {
    if (!window.confirm(`删除目标里的旧副本 ${record.target}？迁回之后它已经不再被使用。`)) {
      return;
    }

    void run(async () => {
      const next = await junctionDiscardTarget(record.id);
      return `${next.source}：${next.note}`;
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

      <div className="card">
        <h3>目录迁移</h3>

        <p className="muted">
          只迁移纯数据目录：把目录复制到其他盘，在原位置留下 junction，原路径照常可访问。
          备份和目标里的旧副本都要你确认之后才删除。
        </p>

        {!dirCheck && (
          <p className="muted">
            在“空间分析”页选一个目录，点“迁移到其他盘…”，这里会显示资格检查结果。
          </p>
        )}

        {dirCheck && (
          <>
            <div className="volume-row">
              <span className="volume-name">待迁移目录</span>
              <span className="path-text" title={dirCheck.path}>
                {dirCheck.path}
              </span>
              <span className="muted">（占盘 {formatSize(dirCheck.allocated)}）</span>
            </div>

            {dirCheck.eligible ? (
              <p className="muted">资格检查通过：没有链接、程序文件、加密文件或云占位符。</p>
            ) : (
              <ul className="warn">
                {dirCheck.reasons.map((reason) => (
                  <li key={reason}>{reason}</li>
                ))}
              </ul>
            )}

            <div className="actions">
              <button
                type="button"
                disabled={busy || planRunning || !dirCheck.eligible || !volume}
                onClick={() => onDirStart(dirCheck)}
              >
                开始迁移
              </button>
              <button
                type="button"
                className="link"
                disabled={busy}
                onClick={() => {
                  setDirCheck(null);
                  onSourceHandled();
                }}
              >
                取消
              </button>
            </div>
          </>
        )}

        {dirs && dirs.records.length > 0 && (
          <table className="table">
            <thead>
              <tr>
                <th>目录</th>
                <th>状态</th>
                <th>步骤</th>
                <th>操作</th>
              </tr>
            </thead>
            <tbody>
              {dirs.records.map((record) => (
                <tr key={record.id}>
                  <td>
                    <div className="path-text" title={record.source}>
                      {record.source}
                    </div>
                    <div className="muted">目标：{record.target}</div>
                    <div className="muted">备份：{record.backup}</div>
                  </td>
                  <td>
                    <span className="status-badge">{record.stageLabel}</span>
                    {record.note && <div className="muted">{record.note}</div>}
                  </td>
                  <td>{timeline(record.stage)}</td>
                  <td>
                    <div className="actions">
                      <button
                        type="button"
                        disabled={busy || planRunning || !record.resumable}
                        onClick={() => onDirContinue(record)}
                      >
                        继续
                      </button>
                      <button
                        type="button"
                        disabled={busy || planRunning || !record.canDeleteBackup}
                        onClick={() => onDirDeleteBackup(record)}
                      >
                        确认删除备份
                      </button>
                      <button
                        type="button"
                        disabled={busy || planRunning || !record.hasJunction}
                        onClick={() => onDirReturn(record)}
                      >
                        迁回
                      </button>
                      <button
                        type="button"
                        // 只有迁回之后目标里的数据才是“旧副本”，迁移成功后它仍是活的数据。
                        disabled={
                          busy ||
                          planRunning ||
                          record.stage !== "returned" ||
                          !record.targetExists
                        }
                        onClick={() => onDirDiscardTarget(record)}
                      >
                        删除旧副本
                      </button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
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

/// 目录迁移的步骤时间线。中断和回滚不在上面的步骤里，单独说明。
function timeline(stage: DirMigrationStage) {
  if (stage === "needsAttention" || stage === "failed" || stage === "returned") {
    const text =
      stage === "returned" ? "已迁回，原位置恢复成普通目录" : stage === "failed" ? "已回滚" : "需要处理";
    return <span className="muted">{text}</span>;
  }

  const current = DIR_STEPS.findIndex(([key]) => key === stage);

  return (
    <ol className="steps">
      {DIR_STEPS.map(([key, label], at) => (
        <li
          key={key}
          className={at === current ? "step step-active" : at < current ? "step step-done" : "step"}
        >
          {label}
        </li>
      ))}
    </ol>
  );
}
