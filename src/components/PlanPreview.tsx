// 执行前的预览确认。清单由后端持有，这里只按计划 ID 取条目、发起执行和取消。
import { Fragment, useEffect, useState } from "react";
import { planCancel, planExecute, planItems, rulesCatalog } from "../api";
import { formatSize, formatTime } from "../format";
import type {
  ExecProgress,
  ExecReport,
  PlanItem,
  PlanRuleSummary,
  PlanSummary,
  Recovery,
} from "../types";

// 清单可能有几十万条，一次全取会把 IPC 撑爆，所以按页取。
const PAGE_SIZE = 200;

// 恢复方式在界面上的叫法。
const RECOVERY_LABELS: Record<Recovery, string> = {
  regenerable: "可再生",
  irreversibleFile: "永久删除",
  quarantine: "隔离（可还原）",
  irreversibleCommand: "不可撤销命令",
  none: "仅展示",
};

interface PlanPreviewProps {
  planId: string;
  // plan_build 的返回值，用来渲染规则行；执行和取条目只用 planId。
  summary: PlanSummary;
  // 执行结束后通知调用方，清理页据此重新评估。
  onFinished?: () => void;
  // 执行状态变化时通知调用方，清理页据此禁用其他执行按钮。
  onRunningChange?: (running: boolean) => void;
  // 其他清理类操作正在进行时，这里不能开始清理。
  blocked?: boolean;
}

export function PlanPreview({
  planId,
  summary,
  onFinished,
  onRunningChange,
  blocked,
}: PlanPreviewProps) {
  // 清单里只有规则 ID，名称取自内置规则目录；取不到就退回显示 ID。
  const [ruleNames, setRuleNames] = useState<Map<string, string>>(new Map());
  const [openRule, setOpenRule] = useState<string | null>(null);
  const [page, setPage] = useState(0);
  const [items, setItems] = useState<PlanItem[]>([]);
  const [itemsLoading, setItemsLoading] = useState(false);
  const [openSkips, setOpenSkips] = useState<string | null>(null);
  // 有不可再生文件时必须勾选，否则不许开始清理。
  const [acknowledged, setAcknowledged] = useState(false);
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState<Record<string, ExecProgress>>({});
  const [report, setReport] = useState<ExecReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    rulesCatalog()
      .then((rules) => setRuleNames(new Map(rules.map((rule) => [rule.id, rule.name]))))
      .catch(() => setRuleNames(new Map()));
  }, []);

  useEffect(() => {
    if (openRule === null) {
      setItems([]);
      return;
    }

    setItemsLoading(true);
    planItems(planId, openRule, page * PAGE_SIZE, PAGE_SIZE)
      .then(setItems)
      .catch((reason) => setError(String(reason)))
      .finally(() => setItemsLoading(false));
  }, [planId, openRule, page]);

  const nameOf = (ruleId: string) => ruleNames.get(ruleId) ?? ruleId;

  const toggleItems = (ruleId: string) => {
    setPage(0);
    setOpenRule(openRule === ruleId ? null : ruleId);
  };

  const execute = () => {
    setRunning(true);
    onRunningChange?.(true);
    setError(null);
    setProgress({});

    planExecute(
      planId,
      summary.rules.map((rule) => rule.ruleId),
      (update) => setProgress((previous) => ({ ...previous, [update.ruleId]: update })),
    )
      .then(setReport)
      .catch((reason) => setError(String(reason)))
      .finally(() => {
        setRunning(false);
        onRunningChange?.(false);
        // 清理已经结束，界面上的候选统计需要重新评估。
        onFinished?.();
      });
  };

  const cancel = () => {
    // 取消只能停下还没处理的条目，已经删掉的收不回来。
    if (!window.confirm("已删除的文件不可恢复，确定要取消这次清理吗？")) {
      return;
    }

    planCancel().catch((reason) => setError(String(reason)));
  };

  const totalLogical = summary.rules.reduce((sum, rule) => sum + rule.logical, 0);
  const totalAllocated = summary.rules.reduce((sum, rule) => sum + rule.allocated, 0);
  const hasIrreversible = summary.rules.some(
    (rule) => rule.recovery === "irreversibleFile",
  );
  // 隔离类规则执行完要引导用户去隔离区页还原或彻底删除。
  const hasQuarantine = summary.rules.some((rule) => rule.recovery === "quarantine");
  const canExecute = !running && !blocked && !report && (!hasIrreversible || acknowledged);
  // 跳过明细可能被后端截断，起点要如实说明。
  const skippedShown = report ? report.skipped.length : 0;

  return (
    <div className="card">
      <h3>清理预览</h3>

      <table className="table">
        <thead>
          <tr>
            <th>名称</th>
            <th>恢复方式</th>
            <th>文件数</th>
            <th>占盘大小</th>
            <th>逻辑大小</th>
            <th>构建时跳过</th>
          </tr>
        </thead>
        <tbody>
          {summary.rules.map((rule) => (
            <Fragment key={rule.ruleId}>
              <tr>
                <td>
                  <button
                    type="button"
                    className="link"
                    onClick={() => toggleItems(rule.ruleId)}
                  >
                    {nameOf(rule.ruleId)}
                  </button>
                </td>
                <td>{RECOVERY_LABELS[rule.recovery]}</td>
                <td>{rule.files.toLocaleString()}</td>
                <td>{formatSize(rule.allocated)}</td>
                <td>{formatSize(rule.logical)}</td>
                <td>{skippedCell(rule, openSkips, setOpenSkips)}</td>
              </tr>

              {openSkips === rule.ruleId && (
                <tr className="sub-row">
                  <td colSpan={6}>
                    <ul className="sample-list">
                      {rule.skippedDetails.map((skipped) => (
                        <li key={`${skipped.path}-${skipped.reason}`}>
                          <span className="path-text" title={skipped.path}>
                            {skipped.path}
                          </span>
                          <span className="muted">{skipped.reason}</span>
                        </li>
                      ))}
                    </ul>
                  </td>
                </tr>
              )}

              {openRule === rule.ruleId && (
                <tr className="sub-row">
                  <td colSpan={6}>
                    {itemsLoading ? (
                      <p className="muted">正在读取条目…</p>
                    ) : (
                      <>
                        <table className="table">
                          <thead>
                            <tr>
                              <th>路径</th>
                              <th>类型</th>
                              <th>占盘大小</th>
                              <th>修改时间</th>
                            </tr>
                          </thead>
                          <tbody>
                            {items.length === 0 && (
                              <tr>
                                <td colSpan={4} className="muted">
                                  这条规则没有清单条目
                                </td>
                              </tr>
                            )}
                            {items.map((item) => (
                              <tr key={item.itemId}>
                                <td className="path-cell" title={item.path}>
                                  {item.path}
                                </td>
                                <td>{item.isDir ? "目录" : "文件"}</td>
                                <td>{formatSize(item.allocated)}</td>
                                <td>{formatTime(item.lastWrite)}</td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                        <div className="pager">
                          <button
                            type="button"
                            disabled={page === 0}
                            onClick={() => setPage(page - 1)}
                          >
                            上一页
                          </button>
                          <span className="muted">
                            共 {rule.items.toLocaleString()} 条，第 {page + 1} /{" "}
                            {pageCount(rule)} 页
                          </span>
                          <button
                            type="button"
                            disabled={page + 1 >= pageCount(rule)}
                            onClick={() => setPage(page + 1)}
                          >
                            下一页
                          </button>
                        </div>
                      </>
                    )}
                  </td>
                </tr>
              )}
            </Fragment>
          ))}
        </tbody>
      </table>

      <dl className="stats">
        <div>
          <dt>逻辑大小</dt>
          <dd>{formatSize(totalLogical)}</dd>
        </div>
        <div>
          <dt>占盘大小</dt>
          <dd>{formatSize(totalAllocated)}</dd>
        </div>
        <div>
          <dt>预计可回收</dt>
          <dd>{formatSize(totalAllocated)}</dd>
        </div>
        {report && (
          <div>
            <dt>执行后空闲变化</dt>
            <dd>{signedSize(report.freeAfter - report.freeBefore)}</dd>
          </div>
        )}
      </dl>
      <p className="muted">硬链接或被占用的文件可能不会释放空间。</p>

      {hasIrreversible && !report && (
        <label className="checkbox-line">
          <input
            type="checkbox"
            checked={acknowledged}
            onChange={(event) => setAcknowledged(event.target.checked)}
          />
          我了解这些文件将被永久删除
        </label>
      )}

      <div className="actions">
        {!report && (
          <button
            type="button"
            className="button-primary"
            disabled={!canExecute}
            onClick={execute}
          >
            {running ? "正在清理…" : "开始清理"}
          </button>
        )}
        {running && (
          <button type="button" onClick={cancel}>
            取消
          </button>
        )}
      </div>

      {error && <p className="warn">{error}</p>}

      {Object.keys(progress).length > 0 && !report && (
        <div className="summary">
          <p className="summary-title">执行进度</p>
          {Object.values(progress).map((entry) => (
            <p key={entry.ruleId} className="muted">
              {nameOf(entry.ruleId)}：已删除 {entry.deletedFiles.toLocaleString()} 个文件（
              {formatSize(entry.freedAllocated)}），跳过 {entry.skipped.toLocaleString()} 个
              {entry.done ? "，已完成" : "，进行中"}
            </p>
          ))}
        </div>
      )}

      {report && (
        <div className="summary">
          <p className="summary-title">执行结果</p>
          {report.aborted && <p className="warn">中止原因：{report.aborted}</p>}
          {report.cancelled && <p className="warn">这次清理被取消，已删除的文件不可恢复。</p>}
          <p className="muted">
            执行前剩余 {formatSize(report.freeBefore)}，执行后剩余{" "}
            {formatSize(report.freeAfter)}。
          </p>
          {hasQuarantine && (
            <p className="muted">
              移入隔离区的条目可以在「隔离区」页查看、还原或彻底删除。
            </p>
          )}

          <table className="table">
            <thead>
              <tr>
                <th>名称</th>
                <th>删除文件</th>
                <th>删除目录</th>
                <th>释放空间</th>
                <th>跳过</th>
              </tr>
            </thead>
            <tbody>
              {report.rules.length === 0 && (
                <tr>
                  <td colSpan={5} className="muted">
                    没有删除任何条目
                  </td>
                </tr>
              )}
              {report.rules.map((rule) => (
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

          {report.skipped.length > 0 && (
            <>
              <p className="muted">
                跳过原因（显示 {skippedShown} 条，共 {report.skippedTotal.toLocaleString()} 条）：
              </p>
              <ul className="sample-list">
                {report.skipped.map((skipped) => (
                  <li key={`${skipped.ruleId}-${skipped.path}`}>
                    <span className="path-text" title={skipped.path}>
                      {skipped.path || nameOf(skipped.ruleId)}
                    </span>
                    <span className="muted">{skipped.reason}</span>
                  </li>
                ))}
              </ul>
            </>
          )}
        </div>
      )}
    </div>
  );
}

// 构建时跳过的单元格：有跳过才给展开按钮。
function skippedCell(
  rule: PlanRuleSummary,
  openSkips: string | null,
  setOpenSkips: (ruleId: string | null) => void,
) {
  if (rule.skippedAtBuild === 0) {
    return "—";
  }

  return (
    <button
      type="button"
      className="link"
      onClick={() => setOpenSkips(openSkips === rule.ruleId ? null : rule.ruleId)}
    >
      {openSkips === rule.ruleId ? "收起" : `跳过 ${rule.skippedAtBuild} 项`}
    </button>
  );
}

function pageCount(rule: PlanRuleSummary): number {
  return Math.max(1, Math.ceil(rule.items / PAGE_SIZE));
}

// 空闲变化带正负号，用户一眼能看出是变多还是变少。
function signedSize(delta: number): string {
  const sign = delta < 0 ? "-" : "+";
  return `${sign}${formatSize(Math.abs(delta))}`;
}
