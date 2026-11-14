// 历史页：按日志列出每次清理的汇总，点开看每条规则的删除数和跳过原因。
import { useEffect, useState } from "react";
import { historyDetail, historyList, rulesCatalog } from "../api";
import { IconHistory } from "../components/Icons";
import { AlertBanner, EmptyState, StatusBadge } from "../components/Ui";
import { formatSize, formatTime } from "../format";
import type { PlanDetail, PlanHistoryEntry } from "../types";

// 历史只读最近这些天的日志文件。
const HISTORY_DAYS = 30;

// 列表里的规则名最多显示几条，剩下的用“等 N 条”带过。
const RULE_NAMES_SHOWN = 5;

export function HistoryPage() {
  const [plans, setPlans] = useState<PlanHistoryEntry[]>([]);
  const [listLoading, setListLoading] = useState(true);
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
      .catch((reason) => setError(String(reason)))
      .finally(() => setListLoading(false));
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
    <section className="page-section">
      <div className="page-header">
        <div>
          <h2 className="page-title">历史</h2>
          <p className="page-subtitle">只读展示最近 {HISTORY_DAYS} 天的执行日志、释放空间与跳过原因</p>
        </div>
      </div>

      {error && <AlertBanner tone="danger">{error}</AlertBanner>}
      {skippedLines > 0 && (
        <AlertBanner tone="warn">
          日志里有 {skippedLines} 行无法解析，下面的历史可能不完整。
        </AlertBanner>
      )}

      {listLoading ? (
        <div className="card">
          <p className="muted">正在加载历史记录…</p>
        </div>
      ) : plans.length === 0 ? (
        <div className="card">
          <EmptyState
            icon={<IconHistory />}
            title="暂无清理记录"
            description={`最近 ${HISTORY_DAYS} 天还没有清理记录。`}
          />
        </div>
      ) : (
        <div className="card">
          <div className="card-header">
            <h3 className="card-title">最近清理记录</h3>
            <span className="muted">点击时间可以查看这次清理的详情</span>
          </div>
          <div className="table-wrap">
            <table className="table">
              <thead>
                <tr>
                  <th>时间</th>
                  <th>状态</th>
                  <th>规则</th>
                  <th>释放空间</th>
                  <th>跳过数</th>
                </tr>
              </thead>
              <tbody>
                {plans.map((plan) => (
                  <tr key={plan.planId}>
                    <td className="td-nowrap">
                      <button
                        type="button"
                        className="btn-link"
                        onClick={() => toggleDetail(plan.planId)}
                      >
                        {formatTime(plan.startedAt)}
                      </button>
                    </td>
                    <td className="td-nowrap">
                      {plan.cancelled ? (
                        <StatusBadge tone="caution">已取消</StatusBadge>
                      ) : !plan.finished ? (
                        <StatusBadge tone="danger">未完成</StatusBadge>
                      ) : (
                        <StatusBadge tone="safe">已正常结束</StatusBadge>
                      )}
                    </td>
                    <td>{ruleSummary(plan, nameOf)}</td>
                    <td className="td-num">{formatSize(plan.freedAllocated)}</td>
                    <td className="td-num">{plan.skipped.toLocaleString()}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}

      {openPlan && (
        <div className="card">
          <div className="card-header">
            <h3 className="card-title">清理详情</h3>
            {detail && (
              <StatusBadge
                tone={detail.cancelled ? "caution" : detail.finished ? "safe" : "danger"}
              >
                {finishLabel(detail)}
              </StatusBadge>
            )}
          </div>

          {loading && <p className="muted">正在读取日志…</p>}

          {detail && (
            <>
              <dl className="stats-grid" style={{ marginBottom: 14 }}>
                <div className="stat-tile">
                  <dt>执行前剩余</dt>
                  <dd>{formatSize(detail.freeBefore)}</dd>
                </div>
                <div className="stat-tile">
                  <dt>执行后剩余</dt>
                  <dd>{formatSize(detail.freeAfter)}</dd>
                </div>
                <div className="stat-tile">
                  <dt>结束状态</dt>
                  <dd>{finishLabel(detail)}</dd>
                </div>
              </dl>

              <div className="table-wrap">
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
                        <td className="td-num">{rule.deletedFiles.toLocaleString()}</td>
                        <td className="td-num">{rule.deletedDirs.toLocaleString()}</td>
                        <td className="td-num">{formatSize(rule.freedAllocated)}</td>
                        <td className="td-num">{rule.skipped.toLocaleString()}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>

              {detail.skipped.length > 0 && (
                <div className="summary-box">
                  <p className="summary-title">
                    跳过原因（显示 {detail.skipped.length} 条，共{" "}
                    {detail.skippedTotal.toLocaleString()} 条）
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
                </div>
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
