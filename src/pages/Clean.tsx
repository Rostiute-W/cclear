import { Fragment, useEffect, useState } from "react";
import {
  candidateLocks,
  openCandidateInExplorer,
  planBuild,
  recycleBinEmpty,
  recycleBinQuery,
  rulesCatalog,
  rulesEvaluate,
} from "../api";
import { PlanPreview } from "../components/PlanPreview";
import { formatDuration, formatSize } from "../format";
import type {
  CandidateInfo,
  CandidateStatusKind,
  LockReport,
  Phase,
  PlanSummary,
  RecycleBinInfo,
  RuleInfo,
  Tier,
} from "../types";

// 分组顺序固定，禁区默认折叠。
const TIER_GROUPS: { tier: Tier; label: string }[] = [
  { tier: "safe", label: "安全" },
  { tier: "caution", label: "谨慎" },
  { tier: "leftover", label: "残留" },
  { tier: "manual", label: "手动" },
  { tier: "forbidden", label: "禁区" },
];

// 徽标上显示阶段时用的写法，和后端 PhaseLocked 的文案保持一致。
const PHASE_LABELS: Record<Phase, string> = {
  analysis: "分析阶段",
  m3a: "M3a",
  m3b: "M3b",
  m4: "M4",
  m5a: "M5a",
  m5b: "M5b",
};

// 单行的占用检查状态。
type LockState =
  | { phase: "loading" }
  | { phase: "done"; report: LockReport }
  | { phase: "failed"; message: string };

export function CleanPage() {
  const [rules, setRules] = useState<RuleInfo[]>([]);
  const [candidates, setCandidates] = useState<Map<string, CandidateInfo>>(new Map());
  const [evaluatedMs, setEvaluatedMs] = useState<number | null>(null);
  const [evaluating, setEvaluating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // 折叠起来的分组。禁区默认折叠，只留名称和原因。
  const [collapsed, setCollapsed] = useState<Set<Tier>>(new Set<Tier>(["forbidden"]));
  // 展开了样例路径的规则。
  const [openSamples, setOpenSamples] = useState<Set<string>>(new Set());
  const [lockStates, setLockStates] = useState<Record<string, LockState>>({});
  // 勾选的规则。每次重新评估后回到默认勾选状态。
  const [selected, setSelected] = useState<Set<string>>(new Set());
  // 生成中的清理预览。清单由后端持有，界面只拿计划 ID。
  const [plan, setPlan] = useState<PlanSummary | null>(null);
  const [planning, setPlanning] = useState(false);
  // C 盘回收站当前内容，清空前要把这份数值回传后端比对。
  const [recycle, setRecycle] = useState<RecycleBinInfo | null>(null);
  const [recycleBusy, setRecycleBusy] = useState(false);

  const refreshRecycle = () => {
    recycleBinQuery()
      .then(setRecycle)
      .catch(() => setRecycle(null));
  };

  const evaluate = () => {
    setEvaluating(true);
    setError(null);
    const started = performance.now();

    rulesEvaluate()
      .then((list) => {
        setCandidates(new Map(list.map((candidate) => [candidate.ruleId, candidate])));
        setEvaluatedMs(performance.now() - started);
        // 文件已经重新统计过，旧的占用结果不再对应当前候选。
        setLockStates({});
      })
      .catch((reason) => setError(String(reason)))
      .finally(() => setEvaluating(false));
  };

  useEffect(() => {
    rulesCatalog()
      .then(setRules)
      .catch((reason) => setError(String(reason)));
    evaluate();
    refreshRecycle();
  }, []);

  // 默认勾选：可执行且默认选中的规则。重新评估后也回到这个状态。
  useEffect(() => {
    if (rules.length === 0) {
      return;
    }

    const defaults = rules
      .filter((rule) => candidates.get(rule.id)?.executable === true && rule.defaultSelected)
      .map((rule) => rule.id);
    setSelected(new Set(defaults));
  }, [rules, candidates]);

  const toggleGroup = (tier: Tier) => {
    setCollapsed((previous) => {
      const next = new Set(previous);
      if (next.has(tier)) {
        next.delete(tier);
      } else {
        next.add(tier);
      }
      return next;
    });
  };

  const toggleSamples = (ruleId: string) => {
    setOpenSamples((previous) => {
      const next = new Set(previous);
      if (next.has(ruleId)) {
        next.delete(ruleId);
      } else {
        next.add(ruleId);
      }
      return next;
    });
  };

  const toggleSelected = (ruleId: string) => {
    setSelected((previous) => {
      const next = new Set(previous);
      if (next.has(ruleId)) {
        next.delete(ruleId);
      } else {
        next.add(ruleId);
      }
      return next;
    });
  };

  const checkLocks = (ruleId: string) => {
    setLockStates((previous) => ({ ...previous, [ruleId]: { phase: "loading" } }));
    candidateLocks(ruleId)
      .then((report) =>
        setLockStates((previous) => ({ ...previous, [ruleId]: { phase: "done", report } })),
      )
      .catch((reason) =>
        setLockStates((previous) => ({
          ...previous,
          [ruleId]: { phase: "failed", message: String(reason) },
        })),
      );
  };

  const openLocation = (ruleId: string, sampleIndex: number | null) => {
    openCandidateInExplorer(ruleId, sampleIndex).catch((reason) => setError(String(reason)));
  };

  // 生成冻结清单后由 PlanPreview 负责确认和执行。
  const startPlan = (ruleIds: string[]) => {
    if (ruleIds.length === 0) {
      return;
    }

    setPlanning(true);
    setError(null);
    planBuild(ruleIds)
      .then(setPlan)
      .catch((reason) => setError(String(reason)))
      .finally(() => setPlanning(false));
  };

  // 某一层里可执行的规则（安全组一键清理用）。
  const executableIds = (tier: Tier) =>
    rules
      .filter((rule) => rule.tier === tier && candidates.get(rule.id)?.executable === true)
      .map((rule) => rule.id);

  // 某一层里用户勾选的规则（谨慎组逐项清理用）。
  const selectedIds = (tier: Tier) =>
    rules
      .filter((rule) => rule.tier === tier && selected.has(rule.id))
      .map((rule) => rule.id);

  // 清空回收站：确认文案里写明执行时清空的是全部内容，并把当前数值一起回传。
  const emptyRecycle = () => {
    if (!recycle) {
      return;
    }

    const text = `将清空执行时 C 盘回收站的全部内容（当前 ${recycle.items} 项，${formatSize(recycle.bytes)}），不可恢复`;
    if (!window.confirm(text)) {
      return;
    }

    setRecycleBusy(true);
    recycleBinEmpty(recycle.items, recycle.bytes)
      .then((info) => {
        setRecycle(info);
        // 清理完自动重新评估，界面上的统计才是清理后的。
        evaluate();
      })
      .catch((reason) => {
        setError(String(reason));
        refreshRecycle();
      })
      .finally(() => setRecycleBusy(false));
  };

  return (
    <section>
      <h2>清理</h2>

      <div className="card">
        <div className="actions">
          <button
            type="button"
            className="button-primary"
            onClick={evaluate}
            disabled={evaluating}
          >
            {evaluating ? "评估中…" : "重新评估"}
          </button>
          {evaluatedMs !== null && (
            <span className="muted">上次评估耗时 {formatDuration(evaluatedMs)}</span>
          )}
        </div>
        <p className="muted">
          只有安全层和谨慎层的文件类规则可以执行，命令类规则仍在后续任务开放。
        </p>
        {error && <p className="warn">{error}</p>}
      </div>

      {TIER_GROUPS.map((group) => {
        const groupRules = rules.filter((rule) => rule.tier === group.tier);
        const isCollapsed = collapsed.has(group.tier);
        // 特殊操作（回收站）不走冻结清单，单独成行，不参与勾选。
        const tableRules = groupRules.filter((rule) => rule.targetKind !== "special");
        const specialRules = groupRules.filter((rule) => rule.targetKind === "special");
        const safeIds = group.tier === "safe" ? executableIds("safe") : [];
        const checkedIds = group.tier === "caution" ? selectedIds("caution") : [];

        return (
          <div className="card" key={group.tier}>
            <div className="group-header">
              <h3>
                {group.label}（{groupRules.length}）
              </h3>
              <div className="actions">
                {group.tier === "safe" && (
                  <button
                    type="button"
                    className="button-primary"
                    disabled={planning || safeIds.length === 0}
                    onClick={() => startPlan(safeIds)}
                  >
                    {planning ? "正在生成清单…" : "一键清理安全项"}
                  </button>
                )}
                {group.tier === "caution" && (
                  <button
                    type="button"
                    className="button-primary"
                    disabled={planning || checkedIds.length === 0}
                    onClick={() => startPlan(checkedIds)}
                  >
                    {planning ? "正在生成清单…" : "清理选中项"}
                  </button>
                )}
                <button type="button" className="link" onClick={() => toggleGroup(group.tier)}>
                  {isCollapsed ? "展开" : "收起"}
                </button>
              </div>
            </div>

            {!isCollapsed &&
              (group.tier === "forbidden" ? (
                // 禁区只列出名称和原因，不提供任何操作。
                <table className="table">
                  <thead>
                    <tr>
                      <th>名称</th>
                      <th>原因</th>
                    </tr>
                  </thead>
                  <tbody>
                    {groupRules.length === 0 && (
                      <tr>
                        <td colSpan={2} className="muted">
                          这一类没有规则
                        </td>
                      </tr>
                    )}
                    {groupRules.map((rule) => (
                      <tr key={rule.id}>
                        <td>{rule.name}</td>
                        <td>{rule.description}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              ) : (
                <>
                  <table className="table">
                    <thead>
                      <tr>
                        <th>选择</th>
                        <th>名称</th>
                        <th>占盘大小</th>
                        <th>状态</th>
                        <th>说明</th>
                        <th>操作</th>
                      </tr>
                    </thead>
                    <tbody>
                      {tableRules.length === 0 && specialRules.length === 0 && (
                        <tr>
                          <td colSpan={6} className="muted">
                            这一类没有规则
                          </td>
                        </tr>
                      )}
                      {tableRules.map((rule) => {
                        const candidate = candidates.get(rule.id);
                        const lockState = lockStates[rule.id];
                        const badge = statusBadge(candidate, rule);
                        const note = statusNote(candidate);
                        const samplesOpen = openSamples.has(rule.id);
                        // 没有锚点时既没有文件可查占用，也没有目录可以打开。
                        const hasAnchors = candidate !== undefined && candidate.anchors.length > 0;
                        // 进程在运行、阶段未开放或证据不足的候选都不能勾选。
                        const selectable = candidate?.executable === true;

                        return (
                          <Fragment key={rule.id}>
                            <tr>
                              <td>
                                <input
                                  type="checkbox"
                                  checked={selected.has(rule.id)}
                                  disabled={!selectable}
                                  onChange={() => toggleSelected(rule.id)}
                                  aria-label={`选择 ${rule.name}`}
                                  title={selectable ? undefined : "当前不能清理这一项"}
                                />
                              </td>
                              <td>{rule.name}</td>
                              <td>{candidate ? formatSize(candidate.allocated) : "—"}</td>
                              <td>
                                {badge ? (
                                  <span className={`status-badge status-${badge.kind}`}>
                                    {badge.label}
                                  </span>
                                ) : (
                                  "—"
                                )}
                              </td>
                              <td>
                                <span>{rule.description}</span>
                                {rule.warning && <p className="warn">注意：{rule.warning}</p>}
                                {note && <p className="muted">{note}</p>}
                              </td>
                              <td>
                                <button
                                  type="button"
                                  className="link"
                                  disabled={!hasAnchors || lockState?.phase === "loading"}
                                  onClick={() => checkLocks(rule.id)}
                                >
                                  检查占用
                                </button>{" "}
                                <button
                                  type="button"
                                  className="link"
                                  disabled={!hasAnchors}
                                  onClick={() => openLocation(rule.id, null)}
                                >
                                  打开位置
                                </button>{" "}
                                {candidate && candidate.samples.length > 0 && (
                                  <button
                                    type="button"
                                    className="link"
                                    onClick={() => toggleSamples(rule.id)}
                                  >
                                    {samplesOpen
                                      ? "收起样例"
                                      : `样例路径（${candidate.samples.length}）`}
                                  </button>
                                )}
                              </td>
                            </tr>

                            {samplesOpen && candidate && (
                              <tr className="sub-row">
                                <td colSpan={6}>
                                  <ul className="sample-list">
                                    {candidate.samples.map((path, index) => (
                                      <li key={path}>
                                        <span className="path-text" title={path}>
                                          {path}
                                        </span>
                                        <button
                                          type="button"
                                          className="link"
                                          onClick={() => openLocation(rule.id, index)}
                                        >
                                          打开
                                        </button>
                                      </li>
                                    ))}
                                  </ul>
                                </td>
                              </tr>
                            )}

                            {lockState && (
                              <tr className="sub-row">
                                <td colSpan={6}>{lockResult(lockState)}</td>
                              </tr>
                            )}
                          </Fragment>
                        );
                      })}

                      {specialRules.map((rule) => (
                        <tr key={rule.id}>
                          <td className="muted">独立操作</td>
                          <td>{rule.name}</td>
                          <td>{recycle ? formatSize(recycle.bytes) : "—"}</td>
                          <td>
                            {recycle ? (
                              <span className="status-badge status-ready">
                                {recycle.items.toLocaleString()} 项
                              </span>
                            ) : (
                              <span className="muted">读取失败</span>
                            )}
                          </td>
                          <td>
                            <span>{rule.description}</span>
                            {rule.warning && <p className="warn">注意：{rule.warning}</p>}
                          </td>
                          <td>
                            <button
                              type="button"
                              className="link"
                              disabled={!recycle || recycleBusy || recycle.items === 0}
                              onClick={emptyRecycle}
                            >
                              {recycleBusy ? "正在清空…" : "清空回收站"}
                            </button>{" "}
                            <button type="button" className="link" onClick={refreshRecycle}>
                              重新查询
                            </button>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </>
              ))}
          </div>
        );
      })}

      {plan && (
        <PlanPreview
          // 每份新计划都重建预览：上一次的执行结果、勾选项和展开状态不能带过来，
          // 否则新计划会沿用旧报告的“已结束”状态而没有“开始清理”按钮。
          key={plan.planId}
          planId={plan.planId}
          summary={plan}
          onFinished={() => {
            // 清理已经结束，把界面上的统计和回收站数量重新读一遍。
            evaluate();
            refreshRecycle();
          }}
        />
      )}
    </section>
  );
}

// 状态徽标。没有评估结果时返回 null，界面显示占位符。
function statusBadge(
  candidate: CandidateInfo | undefined,
  rule: RuleInfo,
): { kind: CandidateStatusKind; label: string } | null {
  if (!candidate) {
    return null;
  }

  switch (candidate.status) {
    case "ready":
      // 阶段还没开放时不能显示“可执行”，否则会让人以为点了就会清理。
      return candidate.executable
        ? { kind: "ready", label: "可执行" }
        : { kind: "phaseLocked", label: `将在 ${PHASE_LABELS[rule.phase]} 开放` };
    case "processRunning":
      return {
        kind: "processRunning",
        label: `进程运行中：${candidate.statusDetail ?? "未知进程"}`,
      };
    case "notPresent":
      return { kind: "notPresent", label: "未找到" };
    case "layoutMismatch":
      return { kind: "layoutMismatch", label: "布局不符" };
    case "phaseLocked":
      return { kind: "phaseLocked", label: candidate.statusDetail ?? "将在后续任务开放" };
    case "insufficient":
      return { kind: "insufficient", label: "证据不足" };
    case "forbidden":
      return { kind: "forbidden", label: "禁区" };
  }
}

// 状态补充说明。开放阶段已经在徽标里，不重复。
function statusNote(candidate: CandidateInfo | undefined): string | null {
  if (!candidate || !candidate.statusDetail) {
    return null;
  }
  if (candidate.status === "phaseLocked") {
    return null;
  }
  // 进程占着文件就删不动，直接告诉用户先退出哪个程序。
  if (candidate.status === "processRunning") {
    return `请先退出 ${candidate.statusDetail}`;
  }

  return candidate.statusDetail;
}

// 占用检查的结果：占用进程列表，或者“未发现占用 / 检测不完整”。
function lockResult(state: LockState) {
  if (state.phase === "loading") {
    return <p className="muted">正在检查占用…</p>;
  }
  if (state.phase === "failed") {
    return <p className="warn">检测失败：{state.message}</p>;
  }

  const { report } = state;
  return (
    <>
      {report.lockers.length > 0 ? (
        <ul className="locker-list">
          {report.lockers.map((locker) => (
            <li key={`${locker.pid}-${locker.appName}`}>
              {locker.appName}（PID {locker.pid}）
              {locker.service ? `，服务 ${locker.service}` : ""}
            </li>
          ))}
        </ul>
      ) : (
        <p className="muted">
          本次未发现占用（检测 {report.checked}/{report.total} 个文件）
        </p>
      )}
      {!report.complete && (
        <p className="warn">检测不完整：{report.error ?? "未知原因"}</p>
      )}
    </>
  );
}
