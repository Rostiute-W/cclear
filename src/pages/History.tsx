// 历史页：按日志列出每次清理的汇总，点开看每条规则的删除数和跳过原因。
import { useEffect, useState } from "react";
import { historyDetail, historyList, rulesCatalog } from "../api";
import { formatSize, formatTime } from "../format";
import type { PlanDetail, PlanHistoryEntry } from "../types";

// 历史只读最近这些天的日志文件。
const HISTORY_DAYS = 30;

// 列表里的规则名最多显示几条，剩下的用“等 N 条”带过。
const RULE_NAMES_SHOWN = 5;

export function HistoryPage() {
  const [plans, setPlans] = useState<PlanHistoryEntry[]>([]);
  const [skippedLines, setSkippedLines] = useState(0);
  // 日志里只有规则 ID，名称取自内置规则目录。
  const [ruleNames, setRuleNames] = useState<Map<string, string>>(new Map());
  const [openPlan, setOpenPlan] = useState<string | null>(null);
  const [detail, setDetail] = useState<PlanDetail | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    rulesCatalog()
      .then((rules) => setRuleNames(new Map(rules.map((rule) => [rule.id, rule.name]))))
      .catch(() => setRuleNames(new Map()));

    historyList(HISTORY_DAYS)
      .then((history) => {
        setPlans(history.plans);
        setSkippedLines(history.skippedLines);
      })
      .catch((reason) => setError(String(reason)));
  }, []);

  const nameOf = (ruleId: string) => ruleNames.get(ruleId) ?? ruleId;

  const toggleDetail = (planId: string) => {
    if (openPlan === planId) {
      setOpenPlan(null);
      setDetail(null);
      return;
    }

    // 详情要重新扫一遍日志，所以只在打开时才读。
    setOpenPlan(planId);
    setDetail(null);
    setLoading(true);
    historyDetail(planId, HISTORY_DAYS)
      .then(setDetail)
      .catch((reason) => setError(String(reason)))
      .finally(() => setLoading(false));
  };

  return (
    <section>
      <h2>历史</h2>

      {error && <p className="warn">{error}</p>}
      {skippedLines > 0 && (
        <p className="warn">
          日志里有 {skippedLines} 行无法解析，下面的历史可能不完整。
        </p>
      )}

      {plans.length === 0 ? (
        <p className="placeholder">最近 {HISTORY_DAYS} 天还没有清理记录。</p>
      ) : (
        <p className="muted">点击时间可以查看这次清理的详情。</p>
      )}

      {plans.length > 0 && (
        <table className="table">
          <thead>
            <tr>
              <th>时间</th>
              <th>规则</th>
              <th>释放空间</th>
              <th>跳过数</th>
            </tr>
          </thead>
          <tbody>
            {plans.map((plan) => (
              <tr key={plan.planId}>
                <td>
                  <button type="button" className="link" onClick={() => toggleDetail(plan.planId)}>
                    {formatTime(plan.startedAt)}
                  </button>
                  {plan.cancelled && <span className="muted">（已取消）</span>}
                  {!plan.finished && !plan.cancelled && <span className="muted">（未完成）</span>}
                </td>
                <td>{ruleSummary(plan, nameOf)}</td>
                <td>{formatSize(plan.freedAllocated)}</td>
                <td>{plan.skipped.toLocaleString()}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {openPlan && (
        <div className="card">
          <h3>清理详情</h3>

          {loading && <p className="muted">正在读取日志…</p>}

          {detail && (
            <>
              <p className="muted">
                执行前剩余 {formatSize(detail.freeBefore)}，执行后剩余{" "}
                {formatSize(detail.freeAfter)}（{finishLabel(detail)}）。
              </p>

              <table className="table">
                <thead>
                  <tr>
                    <th>规则</th>
                    <th>删除文件</th>
                    <th>删除目录</th>
                    <th>释放空间</th>
                    <th>跳过</th>
                  </tr>
                </thead>
                <tbody>
                  {detail.rules.length === 0 && (
                    <tr>
                      <td colSpan={5} className="muted">
                        日志里没有这次清理的规则记录
                      </td>
                    </tr>
                  )}
                  {detail.rules.map((rule) => (
                    <tr key={rule.ruleId}>
                      <td>{nameOf(rule.ruleId)}</td>
                      <td>{rule.deletedFiles.toLocaleString()}</td>
                      <td>{rule.deletedDirs.toLocaleString()}</td>
                      <td>{formatSize(rule.freedAllocated)}</td>
                      <td>{rule.skipped.toLocaleString()}</td>
                    </tr>
                  ))}
                </tbody>
              </table>

              {detail.skipped.length > 0 && (
                <>
                  <p className="muted">
                    跳过原因（显示 {detail.skipped.length} 条，共{" "}
                    {detail.skippedTotal.toLocaleString()} 条）：
                  </p>
                  <ul className="sample-list">
                    {detail.skipped.map((skipped) => (
                      <li key={`${skipped.ruleId}-${skipped.path}-${skipped.reason}`}>
                        <span className="path-text" title={skipped.path}>
                          {skipped.path || nameOf(skipped.ruleId)}
                        </span>
                        <span className="muted">{skipped.reason}</span>
                      </li>
                    ))}
                  </ul>
                </>
              )}
            </>
          )}
        </div>
      )}
    </section>
  );
}

// 结束状态：取消的执行不能显示成“已正常结束”。
function finishLabel(detail: PlanDetail): string {
  if (detail.cancelled) {
    return "已取消";
  }

  return detail.finished ? "已正常结束" : "未正常结束";
}

// “规则”列：列出前几条名称，超出部分只报数量。
function ruleSummary(plan: PlanHistoryEntry, nameOf: (ruleId: string) => string): string {
  if (plan.rules.length === 0) {
    return "—";
  }

  const names = plan.rules.slice(0, RULE_NAMES_SHOWN).map(nameOf).join("、");
  const rest = plan.rules.length - RULE_NAMES_SHOWN;
  return rest > 0 ? `${names} 等 ${plan.rules.length} 条` : names;
}
