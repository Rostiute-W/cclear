import { Fragment, useEffect, useState } from "react";
import {
  candidateLocks,
  openCandidateInExplorer,
  rulesCatalog,
  rulesEvaluate,
} from "../api";
import { formatDuration, formatSize } from "../format";
import type {
  CandidateInfo,
  CandidateStatusKind,
  LockReport,
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
  }, []);

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
        <p className="muted">勾选框暂不可用：执行功能在后续任务开放。</p>
        {error && <p className="warn">{error}</p>}
      </div>

      {TIER_GROUPS.map((group) => {
        const groupRules = rules.filter((rule) => rule.tier === group.tier);
        const isCollapsed = collapsed.has(group.tier);

        return (
          <div className="card" key={group.tier}>
            <div className="group-header">
              <h3>
                {group.label}（{groupRules.length}）
              </h3>
              <button type="button" className="link" onClick={() => toggleGroup(group.tier)}>
                {isCollapsed ? "展开" : "收起"}
              </button>
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
                    {groupRules.length === 0 && (
                      <tr>
                        <td colSpan={6} className="muted">
                          这一类没有规则
                        </td>
                      </tr>
                    )}
                    {groupRules.map((rule) => {
                      const candidate = candidates.get(rule.id);
                      const lockState = lockStates[rule.id];
                      const badge = statusBadge(candidate);
                      const note = statusNote(candidate);
                      const samplesOpen = openSamples.has(rule.id);

                      return (
                        <Fragment key={rule.id}>
                          <tr>
                            <td>
                              <input
                                type="checkbox"
                                disabled
                                aria-label={`选择 ${rule.name}`}
                                title="执行功能在后续任务开放"
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
                                disabled={lockState?.phase === "loading"}
                                onClick={() => checkLocks(rule.id)}
                              >
                                检查占用
                              </button>{" "}
                              <button
                                type="button"
                                className="link"
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
                  </tbody>
                </table>
              ))}
          </div>
        );
      })}
    </section>
  );
}

// 状态徽标。没有评估结果时返回 null，界面显示占位符。
function statusBadge(
  candidate: CandidateInfo | undefined,
): { kind: CandidateStatusKind; label: string } | null {
  if (!candidate) {
    return null;
  }

  switch (candidate.status) {
    case "ready":
      return { kind: "ready", label: "可执行" };
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

// 状态补充说明。进程名和开放阶段已经在徽标里，不重复。
function statusNote(candidate: CandidateInfo | undefined): string | null {
  if (!candidate || !candidate.statusDetail) {
    return null;
  }
  if (candidate.status === "processRunning" || candidate.status === "phaseLocked") {
    return null;
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
