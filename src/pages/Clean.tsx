import { Fragment, useEffect, useState, type FormEvent } from "react";
import {
  aiAskRule,
  candidateLocks,
  commandExecute,
  elevatedExecute,
  maintenanceAnalyze,
  maintenanceExecute,
  openCandidateInExplorer,
  planBuild,
  recycleBinEmpty,
  recycleBinQuery,
  rulesCatalog,
  rulesEvaluate,
} from "../api";
import {
  IconInfo,
  IconLock,
  IconQuarantine,
  IconRefresh,
  IconShield,
  IconTerminal,
  IconWarning,
} from "../components/Icons";
import { PlanPreview } from "../components/PlanPreview";
import { AdminTag, AlertBanner, StatusBadge, type SemanticTone } from "../components/Ui";
import { formatDuration, formatSize } from "../format";
import type {
  AiAnswer,
  CandidateInfo,
  CandidateStatusKind,
  CommandReport,
  ElevatedResult,
  LockReport,
  MaintenanceProgress,
  Phase,
  PlanSummary,
  RecycleBinInfo,
  RuleInfo,
  Tier,
} from "../types";

// 分组顺序固定，禁区默认折叠。
const TIER_GROUPS: { tier: Tier; label: string; hint: string }[] = [
  { tier: "safe", label: "安全", hint: "可安全清理或自动再生" },
  { tier: "caution", label: "谨慎", hint: "逐项确认后再清理" },
  { tier: "leftover", label: "残留", hint: "先移入隔离区 · 可还原" },
  { tier: "manual", label: "手动", hint: "仅分析参考或阶段受限" },
  { tier: "forbidden", label: "禁区", hint: "系统核心与不可触碰路径" },
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

const CANDIDATE_TONES: Record<CandidateStatusKind, SemanticTone> = {
  ready: "safe",
  processRunning: "caution",
  notPresent: "neutral",
  layoutMismatch: "neutral",
  phaseLocked: "info",
  insufficient: "neutral",
  forbidden: "danger",
};

// 单行的占用检查状态。
type LockState =
  | { phase: "loading" }
  | { phase: "done"; report: LockReport }
  | { phase: "failed"; message: string };

// 能按冻结清单执行的规则。命令和特殊操作各有自己的执行方式，不进清单。
function isPlanRule(rule: RuleInfo): boolean {
  return rule.targetKind !== "command" && rule.targetKind !== "special";
}

// 有内置操作的系统项只在“系统维护”分组里出现，不再进分级表格。
function isMaintenanceRule(rule: RuleInfo): boolean {
  return rule.systemOp !== null;
}

// 内置操作名到界面文案。后端返回的是 kebab-case 的操作名。
const SYSTEM_OP_LABELS: Record<string, string> = {
  "windows-temp": "Windows\\Temp",
  "windows-logs": "Windows\\Logs",
  "sys-reset": "$SysReset",
  "dism-analyze": "组件存储分析",
  "dism-component-cleanup": "组件存储清理",
  "delivery-optimization": "传递优化缓存",
};

function systemOpLabel(op: string): string {
  return SYSTEM_OP_LABELS[op] ?? op;
}

function tierBadge(tier: Tier, hint: string) {
  switch (tier) {
    case "safe":
      return (
        <StatusBadge tone="safe" icon={<IconShield width={12} height={12} />}>
          {hint}
        </StatusBadge>
      );
    case "caution":
      return (
        <StatusBadge tone="caution" icon={<IconWarning width={12} height={12} />}>
          {hint}
        </StatusBadge>
      );
    case "leftover":
      return (
        <StatusBadge tone="quarantine" icon={<IconQuarantine width={12} height={12} />}>
          {hint}
        </StatusBadge>
      );
    case "forbidden":
      return (
        <StatusBadge tone="danger" icon={<IconLock width={12} height={12} />}>
          {hint}
        </StatusBadge>
      );
    case "manual":
      return (
        <StatusBadge tone="neutral" icon={<IconInfo width={12} height={12} />}>
          {hint}
        </StatusBadge>
      );
  }
}

// 单条命令的运行状态。
type CommandState =
  | { phase: "running" }
  | { phase: "done"; report: CommandReport }
  | { phase: "failed"; message: string };

// 单个需要管理员权限的清理项的执行状态。
type ElevatedState =
  | { phase: "running" }
  | { phase: "done"; result: ElevatedResult }
  | { phase: "failed"; message: string };

export function CleanPage({ onOpenSettings }: { onOpenSettings: () => void }) {
  const [rules, setRules] = useState<RuleInfo[]>([]);
  const [candidates, setCandidates] = useState<Map<string, CandidateInfo>>(new Map());
  const [evaluatedMs, setEvaluatedMs] = useState<number | null>(null);
  const [evaluating, setEvaluating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // 折叠起来的分组。禁区默认折叠，只留名称和原因。
  const [collapsed, setCollapsed] = useState<Set<Tier>>(new Set<Tier>(["forbidden"]));
  // 展开了样例路径的规则。
  const [openSamples, setOpenSamples] = useState<Set<string>>(new Set());
  // 展开了判定证据的规则。残留规则靠证据说明为什么选中这些目录。
  const [openEvidence, setOpenEvidence] = useState<Set<string>>(new Set());
  const [lockStates, setLockStates] = useState<Record<string, LockState>>({});
  // 清理页共享一个“问 AI”面板与请求锁，同一时间只允许发一条提问。
  const [askRuleId, setAskRuleId] = useState<string | null>(null);
  const [askQuestion, setAskQuestion] = useState("");
  const [askBusy, setAskBusy] = useState(false);
  const [askAnswer, setAskAnswer] = useState<AiAnswer | null>(null);
  const [askError, setAskError] = useState<string | null>(null);
  const [askSentOpen, setAskSentOpen] = useState(false);
  // 勾选的规则。每次重新评估后回到默认勾选状态。
  const [selected, setSelected] = useState<Set<string>>(new Set());
  // 生成中的清理预览。清单由后端持有，界面只拿计划 ID。
  const [plan, setPlan] = useState<PlanSummary | null>(null);
  const [planning, setPlanning] = useState(false);
  // C 盘回收站当前内容，清空前要把这份数值回传后端比对。
  const [recycle, setRecycle] = useState<RecycleBinInfo | null>(null);
  const [recycleBusy, setRecycleBusy] = useState(false);
  // 清单是否正在执行。执行状态在 PlanPreview 内部，这里留一份用来禁用其他执行按钮。
  const [planRunning, setPlanRunning] = useState(false);
  // 每条命令的运行状态。命令之间互不影响，按规则 ID 分开记。
  const [commandStates, setCommandStates] = useState<Record<string, CommandState>>({});
  // 需要管理员权限的清理项的执行状态，同样按规则 ID 分开记。
  const [elevatedStates, setElevatedStates] = useState<Record<string, ElevatedState>>({});
  // 系统维护分组的勾选项。默认全不选，避免误删系统文件。
  const [maintenanceSelected, setMaintenanceSelected] = useState<Set<string>>(new Set());
  // 组件存储分析（只读）的状态。
  const [analyzeState, setAnalyzeState] = useState<ElevatedState | null>(null);
  // 系统维护执行中、最近一条进度和最终结果。
  const [maintenanceRunning, setMaintenanceRunning] = useState(false);
  const [maintenanceProgress, setMaintenanceProgress] = useState<MaintenanceProgress | null>(null);
  const [maintenanceResult, setMaintenanceResult] = useState<ElevatedResult | null>(null);
  const [maintenanceError, setMaintenanceError] = useState<string | null>(null);

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

  const toggleEvidence = (ruleId: string) => {
    setOpenEvidence((previous) => {
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

  const toggleAsk = (ruleId: string) => {
    if (askBusy) {
      return;
    }
    if (askRuleId === ruleId) {
      setAskRuleId(null);
      return;
    }
    setAskRuleId(ruleId);
    setAskQuestion("");
    setAskAnswer(null);
    setAskError(null);
    setAskSentOpen(false);
  };

  const submitAsk = (event: FormEvent) => {
    event.preventDefault();
    if (!askRuleId || askBusy) {
      return;
    }
    const trimmed = askQuestion.trim();
    setAskBusy(true);
    setAskAnswer(null);
    setAskError(null);
    setAskSentOpen(false);

    aiAskRule(askRuleId, trimmed === "" ? null : trimmed)
      .then(setAskAnswer)
      .catch((reason) => setAskError(String(reason)))
      .finally(() => setAskBusy(false));
  };

  const renderAskButton = (ruleId: string) => (
    <button
      type="button"
      className="btn-link"
      disabled={askBusy}
      onClick={() => toggleAsk(ruleId)}
    >
      {askRuleId === ruleId ? "收起 AI" : "问 AI"}
    </button>
  );

  const renderAskSubRow = (ruleId: string, colSpan: number) => {
    if (askRuleId !== ruleId) {
      return null;
    }

    return (
      <tr className="sub-row">
        <td colSpan={colSpan}>
          <div className="ai-ask-box">
            <form className="ai-ask-form" onSubmit={submitAsk}>
              <input
                type="text"
                className="text-input"
                value={askQuestion}
                disabled={askBusy}
                placeholder="可选：补充你想问的，例如‘我还在用 VS Code，能删吗？’"
                onChange={(event) => setAskQuestion(event.target.value)}
              />
              <button type="submit" className="btn btn-primary" disabled={askBusy}>
                {askBusy ? "发送中…" : "发送"}
              </button>
            </form>

            {askBusy && (
              <p className="muted" style={{ margin: 0 }}>
                正在询问 AI…
              </p>
            )}

            {askError && (
              <div className="ai-ask-error">
                <span className="danger-text">{askError}</span>
                <button type="button" className="btn btn-subtle" onClick={onOpenSettings}>
                  去设置
                </button>
              </div>
            )}

            {askAnswer && (
              <div className="ai-answer-box">
                <div className="ai-answer-meta">
                  <StatusBadge tone="info">{askAnswer.model}</StatusBadge>
                  <span className="muted">仅供参考</span>
                  <button
                    type="button"
                    className="btn-link"
                    onClick={() => setAskSentOpen((previous) => !previous)}
                  >
                    {askSentOpen ? "收起发送内容" : "查看发送内容"}
                  </button>
                </div>
                <div className="ai-answer-text">{askAnswer.answer}</div>
                {askSentOpen && <pre className="ai-sent-pre">{askAnswer.sent}</pre>}
              </div>
            )}
          </div>
        </td>
      </tr>
    );
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

  // 某一层里可执行的规则（安全组一键清理用）。命令和特殊操作不走冻结清单，排除掉。
  const executableIds = (tier: Tier) =>
    rules
      .filter(
        (rule) =>
          rule.tier === tier && isPlanRule(rule) && candidates.get(rule.id)?.executable === true,
      )
      .map((rule) => rule.id);

  // 某一层里用户勾选的规则（谨慎组逐项清理用）。同样只保留按清单执行的规则。
  const selectedIds = (tier: Tier) =>
    rules
      .filter((rule) => rule.tier === tier && isPlanRule(rule) && selected.has(rule.id))
      .map((rule) => rule.id);

  // 运行官方命令：确认文案写清工具和作用范围，需要二次确认的规则多问一步。
  const runCommand = (rule: RuleInfo, scope: string) => {
    const tool = rule.command?.tool ?? rule.name;
    const text = `调用 ${tool} 官方命令，作用范围：${scope}；不可撤销；无法逐文件预览`;
    if (!window.confirm(text)) {
      return;
    }

    const needSecond = rule.command?.needsSecondConfirm === true;
    const secondText = `「${rule.name}」会删除全部缓存，下次使用需要重新下载。确认继续？`;
    if (needSecond && !window.confirm(secondText)) {
      return;
    }

    setCommandStates((previous) => ({ ...previous, [rule.id]: { phase: "running" } }));
    commandExecute(rule.id, needSecond, (progress) => {
      // 命令一开始就保持“运行中”，结束后由返回值接管。
      if (progress.stage === "running") {
        setCommandStates((previous) => ({ ...previous, [rule.id]: { phase: "running" } }));
      }
    })
      .then((report) => {
        setCommandStates((previous) => ({ ...previous, [rule.id]: { phase: "done", report } }));
        // 命令结束后缓存大小变了，重新评估一次，界面上的数字才是当前的。
        evaluate();
      })
      .catch((reason) => {
        setCommandStates((previous) => ({
          ...previous,
          [rule.id]: { phase: "failed", message: String(reason) },
        }));
      });
  };

  // 需要管理员权限的清理项：一次调用可以带多条规则，helper 只弹一次授权。
  const runElevated = (targets: RuleInfo[]) => {
    if (targets.length === 0) {
      return;
    }

    const names = targets.map((rule) => `「${rule.name}」`).join("、");
    const text = `${names} 需要管理员权限，执行时会弹出一次授权提示；清理结果先进入隔离区。确认继续？`;
    if (!window.confirm(text)) {
      return;
    }

    const ruleIds = targets.map((rule) => rule.id);
    setElevatedStates((previous) => {
      const next = { ...previous };
      for (const ruleId of ruleIds) {
        next[ruleId] = { phase: "running" };
      }
      return next;
    });

    elevatedExecute(ruleIds)
      .then((result) => {
        setElevatedStates((previous) => {
          const next = { ...previous };
          // 结果与传入的规则按顺序一一对应，每条规则只显示属于自己的那一项。
          ruleIds.forEach((ruleId, index) => {
            const op = result.ops[index];
            next[ruleId] = op
              ? { phase: "done", result: { ops: [op] } }
              : { phase: "failed", message: "提权 helper 没有返回这一项的结果" };
          });
          return next;
        });
        // 内容已经移入隔离区，重新评估一次，界面上的数字才是当前的。
        evaluate();
      })
      .catch((reason) => {
        setElevatedStates((previous) => {
          const next = { ...previous };
          for (const ruleId of ruleIds) {
            next[ruleId] = { phase: "failed", message: String(reason) };
          }
          return next;
        });
      });
  };

  // 系统维护分组：勾选、只读分析，以及一次授权执行全部选中项。
  const toggleMaintenance = (ruleId: string) => {
    setMaintenanceSelected((previous) => {
      const next = new Set(previous);
      if (next.has(ruleId)) {
        next.delete(ruleId);
      } else {
        next.add(ruleId);
      }
      return next;
    });
  };

  // 分析组件存储：只读，不改动任何内容，但同样要管理员权限。
  const analyzeComponentStore = () => {
    setAnalyzeState({ phase: "running" });
    maintenanceAnalyze()
      .then((result) => setAnalyzeState({ phase: "done", result }))
      .catch((reason) =>
        setAnalyzeState({ phase: "failed", message: String(reason) }),
      );
  };

  // 执行选中的系统维护项。全部选中项合成一次提权调用，只弹一次授权。
  const runMaintenance = (ruleIds: string[]) => {
    if (ruleIds.length === 0) {
      return;
    }

    const text = `将执行选中的 ${ruleIds.length} 项系统维护操作，只会弹出一次授权提示；其中组件存储清理（DISM）可能需要 10–30 分钟且无法取消，请勿关机；这些操作不可撤销。确认继续？`;
    if (!window.confirm(text)) {
      return;
    }

    setMaintenanceRunning(true);
    setMaintenanceProgress(null);
    setMaintenanceResult(null);
    setMaintenanceError(null);

    maintenanceExecute(ruleIds, setMaintenanceProgress)
      .then((result) => {
        setMaintenanceResult(result);
        // 系统文件已经清过，重新评估一次，界面上的数字才是当前的。
        evaluate();
      })
      .catch((reason) => setMaintenanceError(String(reason)))
      .finally(() => {
        setMaintenanceProgress(null);
        setMaintenanceRunning(false);
      });
  };

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

  // 命令类规则单独成组：它们不生成冻结清单，一条一行单独运行。
  const commandRules = rules.filter((rule) => rule.targetKind === "command");

  // 有内置操作的系统项。还没有内置操作的项留在分级表格里显示“阶段未开放”。
  const maintenanceRules = rules.filter(isMaintenanceRule);
  const maintenanceIds = maintenanceRules
    .filter((rule) => maintenanceSelected.has(rule.id))
    .map((rule) => rule.id);

  // 残留层里需要管理员权限、当前可以执行的规则。合成一次提权调用，只弹一次授权。
  const adminLeftoverRules = rules.filter(
    (rule) =>
      rule.tier === "leftover" &&
      rule.admin &&
      isPlanRule(rule) &&
      candidates.get(rule.id)?.executable === true,
  );

  // 命令、回收站和清单共用后端的执行锁：任一操作进行时，其他执行按钮都禁用。
  // 判断以后端为准，这里只是不让用户白点。
  const anyCommandRunning = Object.values(commandStates).some(
    (state) => state.phase === "running",
  );
  const anyElevatedRunning = Object.values(elevatedStates).some(
    (state) => state.phase === "running",
  );
  const anyAnalyzeRunning = analyzeState?.phase === "running";
  const execBusy =
    planning ||
    recycleBusy ||
    planRunning ||
    anyCommandRunning ||
    anyElevatedRunning ||
    anyAnalyzeRunning ||
    maintenanceRunning;

  return (
    <section className="page-section">
      <div className="page-header">
        <div>
          <h2 className="page-title">清理</h2>
          <p className="page-subtitle">先评估各规则候选规模，生成冻结清单预览后再确认执行</p>
        </div>
        <div className="actions">
          {evaluatedMs !== null && (
            <StatusBadge tone="neutral">上次评估耗时 {formatDuration(evaluatedMs)}</StatusBadge>
          )}
          <button
            type="button"
            className="btn btn-primary"
            onClick={evaluate}
            disabled={evaluating}
          >
            <IconRefresh width={14} height={14} />
            <span>{evaluating ? "评估中…" : "重新评估"}</span>
          </button>
        </div>
      </div>

      <AlertBanner tone="info">
        安全层可一键清理，谨慎层逐项勾选后清理，残留层的条目一律先进隔离区。命令类规则在下方「不可撤销命令」分组里单独运行。
      </AlertBanner>

      {error && <AlertBanner tone="danger">{error}</AlertBanner>}

      {TIER_GROUPS.map((group) => {
        const groupRules = rules.filter((rule) => rule.tier === group.tier);
        const isCollapsed = collapsed.has(group.tier);
        // 特殊操作（回收站）和命令都不走冻结清单，单独成行，不参与勾选。
        // 系统维护项有内置操作，放到下面的“系统维护”分组里。
        const tableRules = groupRules.filter(
          (rule) => isPlanRule(rule) && !isMaintenanceRule(rule),
        );
        const specialRules = groupRules.filter((rule) => rule.targetKind === "special");
        // 卡片标题按实际渲染的条目数统计：命令和特殊操作已经移出各自的表格。
        const cardCount =
          group.tier === "forbidden" ? groupRules.length : tableRules.length + specialRules.length;
        const safeIds = group.tier === "safe" ? executableIds("safe") : [];
        const checkedIds = group.tier === "caution" ? selectedIds("caution") : [];

        return (
          <div className={`card card-tier-${group.tier}`} key={group.tier}>
            <div className="card-header" style={isCollapsed ? { marginBottom: 0 } : undefined}>
              <div className="card-title-group">
                <h3 className="card-title">
                  {group.label}（{cardCount}）
                </h3>
                {tierBadge(group.tier, group.hint)}
              </div>
              <div className="actions">
                {group.tier === "safe" && (
                  <button
                    type="button"
                    className="btn btn-primary"
                    disabled={execBusy || safeIds.length === 0}
                    onClick={() => startPlan(safeIds)}
                  >
                    {planning ? "正在生成清单…" : "一键清理安全项"}
                  </button>
                )}
                {group.tier === "caution" && (
                  <button
                    type="button"
                    className="btn btn-primary"
                    disabled={execBusy || checkedIds.length === 0}
                    onClick={() => startPlan(checkedIds)}
                  >
                    {planning ? "正在生成清单…" : "清理选中项"}
                  </button>
                )}
                {group.tier === "leftover" && adminLeftoverRules.length > 0 && (
                  <button
                    type="button"
                    className="btn btn-primary"
                    disabled={execBusy}
                    onClick={() => runElevated(adminLeftoverRules)}
                  >
                    可执行的管理员残留项一起隔离（一次授权）
                  </button>
                )}
                <button
                  type="button"
                  className="btn btn-subtle"
                  onClick={() => toggleGroup(group.tier)}
                >
                  {isCollapsed ? "展开" : "收起"}
                </button>
              </div>
            </div>

            {!isCollapsed &&
              (group.tier === "forbidden" ? (
                // 禁区只列出名称和原因，不提供任何操作。
                <div className="table-wrap">
                  <table className="table">
                    <thead>
                      <tr>
                        <th>名称</th>
                        <th>原因</th>
                        <th>操作</th>
                      </tr>
                    </thead>
                    <tbody>
                      {groupRules.length === 0 && (
                        <tr>
                          <td colSpan={3} className="muted">
                            这一类没有规则
                          </td>
                        </tr>
                      )}
                      {groupRules.map((rule) => (
                        <Fragment key={rule.id}>
                          <tr>
                            <td className="td-nowrap">{rule.name}</td>
                            <td>{rule.description}</td>
                            <td className="td-ops">
                              <div className="actions">{renderAskButton(rule.id)}</div>
                            </td>
                          </tr>
                          {renderAskSubRow(rule.id, 3)}
                        </Fragment>
                      ))}
                    </tbody>
                  </table>
                </div>
              ) : (
                <div className="table-wrap">
                  <table className="table rule-table">
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
                        const evidenceOpen = openEvidence.has(rule.id);
                        const elevated = elevatedStates[rule.id];
                        // 没有锚点时既没有文件可查占用，也没有目录可以打开。
                        const hasAnchors = candidate !== undefined && candidate.anchors.length > 0;
                        // 进程在运行、阶段未开放或证据不足的候选都不能勾选。
                        const selectable = candidate?.executable === true;
                        // 证据列表和没有被选中的原因，两者都没有时不给展开按钮。
                        const hasEvidence =
                          candidate !== undefined &&
                          (candidate.evidence.length > 0 || candidate.notes.length > 0);

                        return (
                          <Fragment key={rule.id}>
                            <tr>
                              <td className="td-nowrap">
                                {group.tier === "leftover" ? (
                                  // 残留项一条操作一次，勾选对它没有意义。
                                  <StatusBadge tone="quarantine">隔离</StatusBadge>
                                ) : (
                                  <input
                                    type="checkbox"
                                    className="checkbox-input"
                                    checked={selected.has(rule.id)}
                                    disabled={!selectable}
                                    onChange={() => toggleSelected(rule.id)}
                                    aria-label={`选择 ${rule.name}`}
                                    title={selectable ? undefined : "当前不能清理这一项"}
                                  />
                                )}
                              </td>
                              <td className="td-rule-name">{rule.name}</td>
                              <td className="td-num">
                                {candidate ? formatSize(candidate.allocated) : "—"}
                              </td>
                              <td className="td-status">
                                {badge ? (
                                  <StatusBadge tone={CANDIDATE_TONES[badge.kind]}>
                                    {badge.label}
                                  </StatusBadge>
                                ) : (
                                  "—"
                                )}
                              </td>
                              <td className="td-desc">
                                <div className="actions" style={{ marginBottom: rule.admin ? 4 : 0 }}>
                                  <span>{rule.description}</span>
                                  {rule.admin && <AdminTag />}
                                </div>
                                {rule.warning && <p className="warn-text">注意：{rule.warning}</p>}
                                {note && <p className="muted" style={{ margin: "3px 0 0" }}>{note}</p>}
                              </td>
                              <td className="td-ops">
                                <div className="actions">
                                  <button
                                    type="button"
                                    className="btn-link"
                                    disabled={!hasAnchors || lockState?.phase === "loading"}
                                    onClick={() => checkLocks(rule.id)}
                                  >
                                    检查占用
                                  </button>
                                  <button
                                    type="button"
                                    className="btn-link"
                                    disabled={!hasAnchors}
                                    onClick={() => openLocation(rule.id, null)}
                                  >
                                    打开位置
                                  </button>
                                  {candidate && candidate.samples.length > 0 && (
                                    <button
                                      type="button"
                                      className="btn-link"
                                      onClick={() => toggleSamples(rule.id)}
                                    >
                                      {samplesOpen
                                        ? "收起样例"
                                        : `样例路径（${candidate.samples.length}）`}
                                    </button>
                                  )}
                                  {hasEvidence && (
                                    <button
                                      type="button"
                                      className="btn-link"
                                      onClick={() => toggleEvidence(rule.id)}
                                    >
                                      {evidenceOpen ? "收起证据" : "查看证据"}
                                    </button>
                                  )}
                                  {renderAskButton(rule.id)}
                                  {group.tier === "leftover" && (
                                    <button
                                      type="button"
                                      className="btn btn-primary"
                                      disabled={!selectable || execBusy}
                                      onClick={() =>
                                        rule.admin ? runElevated([rule]) : startPlan([rule.id])
                                      }
                                      title={selectable ? undefined : "当前不能清理这一项"}
                                    >
                                      {rule.admin ? "移入隔离区（需管理员）" : "移入隔离区"}
                                    </button>
                                  )}
                                </div>
                              </td>
                            </tr>

                            {evidenceOpen && candidate && (
                              <tr className="sub-row">
                                <td colSpan={6}>
                                  <p className="muted" style={{ margin: 0 }}>
                                    判定依据：没有列在这里的内容不会被处理。
                                  </p>
                                  <ul className="sample-list">
                                    {candidate.evidence.map((line) => (
                                      <li key={line}>
                                        <span className="path-text" title={line}>
                                          {line}
                                        </span>
                                      </li>
                                    ))}
                                  </ul>
                                  {candidate.notes.map((line) => (
                                    <p key={line} className="muted" style={{ margin: "4px 0 0" }}>
                                      {line}
                                    </p>
                                  ))}
                                </td>
                              </tr>
                            )}

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
                                          className="btn-link"
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

                            {elevated && (
                              <tr className="sub-row">
                                <td colSpan={6}>{elevatedResult(elevated)}</td>
                              </tr>
                            )}
                            {renderAskSubRow(rule.id, 6)}
                          </Fragment>
                        );
                      })}

                      {specialRules.map((rule) => (
                        <Fragment key={rule.id}>
                          <tr>
                            <td className="td-nowrap">
                              <StatusBadge tone="danger">独立操作</StatusBadge>
                            </td>
                            <td className="td-rule-name">{rule.name}</td>
                            <td className="td-num">{recycle ? formatSize(recycle.bytes) : "—"}</td>
                            <td className="td-nowrap">
                              {recycle ? (
                                <StatusBadge tone="neutral">
                                  {recycle.items.toLocaleString()} 项
                                </StatusBadge>
                              ) : (
                                <StatusBadge tone="neutral">读取失败</StatusBadge>
                              )}
                            </td>
                            <td className="td-desc">
                              <span>{rule.description}</span>
                              {rule.warning && <p className="warn-text">注意：{rule.warning}</p>}
                            </td>
                            <td className="td-ops">
                              <div className="actions">
                                <button
                                  type="button"
                                  className="btn btn-danger-outline"
                                  disabled={!recycle || execBusy || recycle.items === 0}
                                  onClick={emptyRecycle}
                                >
                                  {recycleBusy ? "正在清空…" : "清空回收站"}
                                </button>
                                <button
                                  type="button"
                                  className="btn btn-subtle"
                                  onClick={refreshRecycle}
                                >
                                  重新查询
                                </button>
                                {renderAskButton(rule.id)}
                              </div>
                            </td>
                          </tr>
                          {renderAskSubRow(rule.id, 6)}
                        </Fragment>
                      ))}
                    </tbody>
                  </table>
                </div>
              ))}
          </div>
        );
      })}

      {maintenanceRules.length > 0 && (
        <div className="card card-tier-caution">
          <div className="card-header">
            <div className="card-title-group">
              <h3 className="card-title">系统维护（需要管理员）（{maintenanceRules.length}）</h3>
              <AdminTag />
            </div>
            <div className="actions">
              <button
                type="button"
                className="btn btn-subtle"
                disabled={execBusy}
                onClick={analyzeComponentStore}
              >
                {anyAnalyzeRunning ? "正在分析…" : "分析组件存储（只读）"}
              </button>
              <button
                type="button"
                className="btn btn-danger-solid"
                disabled={execBusy || maintenanceIds.length === 0}
                onClick={() => runMaintenance(maintenanceIds)}
              >
                {maintenanceRunning ? "正在执行…" : "执行选中项"}
              </button>
            </div>
          </div>

          <div style={{ marginBottom: 12 }}>
            <AlertBanner tone="warn">
              这些项走 Windows 自己的维护流程，需要管理员权限。勾选多项后点“执行选中项”，整个过程中只会弹出一次授权提示。
              文件类操作按冻结清单逐项删除，被占用的文件跳过；DISM 组件存储清理不做 ResetBase，可能需要 10–30
              分钟且无法取消，请勿关机；传递优化缓存调用官方 cmdlet。
            </AlertBanner>
          </div>

          <div className="table-wrap">
            <table className="table">
              <thead>
                <tr>
                  <th>选择</th>
                  <th>名称</th>
                  <th>说明</th>
                  <th>操作</th>
                </tr>
              </thead>
              <tbody>
                {maintenanceRules.map((rule) => (
                  <Fragment key={rule.id}>
                    <tr>
                      <td className="td-nowrap">
                        <input
                          type="checkbox"
                          className="checkbox-input"
                          checked={maintenanceSelected.has(rule.id)}
                          disabled={maintenanceRunning || anyAnalyzeRunning}
                          onChange={() => toggleMaintenance(rule.id)}
                          aria-label={`选择 ${rule.name}`}
                        />
                      </td>
                      <td className="td-nowrap">{rule.name}</td>
                      <td>
                        <span>{rule.description}</span>
                        {rule.warning && <p className="warn-text">注意：{rule.warning}</p>}
                      </td>
                      <td className="td-ops">
                        <div className="actions">{renderAskButton(rule.id)}</div>
                      </td>
                    </tr>
                    {renderAskSubRow(rule.id, 4)}
                  </Fragment>
                ))}
              </tbody>
            </table>
          </div>

          {anyAnalyzeRunning && (
            <p className="muted" style={{ marginTop: 10 }}>
              正在分析组件存储，可能需要几分钟…
            </p>
          )}
          {analyzeState?.phase === "failed" && (
            <div style={{ marginTop: 10 }}>
              <AlertBanner tone="danger">分析失败：{analyzeState.message}</AlertBanner>
            </div>
          )}
          {analyzeState?.phase === "done" && (
            <div className="summary-box">
              {analyzeState.result.ops.map((op) => (
                <p
                  key={op.op}
                  className={op.status === "failed" ? "danger-text" : "muted"}
                  style={{ margin: "3px 0 0" }}
                >
                  {systemOpLabel(op.op)}：{op.detail}
                </p>
              ))}
            </div>
          )}

          {maintenanceRunning && (
            <p className="muted" style={{ marginTop: 10 }}>
              {maintenanceProgress
                ? `${systemOpLabel(maintenanceProgress.op)}：${maintenanceProgress.detail}`
                : "等待管理员授权并执行…"}
            </p>
          )}
          {maintenanceError && (
            <div style={{ marginTop: 10 }}>
              <AlertBanner tone="danger">系统维护失败：{maintenanceError}</AlertBanner>
            </div>
          )}
          {maintenanceResult && (
            <div className="summary-box">
              <ul className="sample-list" style={{ marginTop: 0 }}>
                {maintenanceResult.ops.map((op) => (
                  <li key={op.op} className={op.status === "failed" ? "danger-text" : "muted"}>
                    {systemOpLabel(op.op)}：{op.detail}
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>
      )}

      {commandRules.length > 0 && (
        <div className="card card-tier-forbidden">
          <div className="card-header">
            <div className="card-title-group">
              <h3 className="card-title">不可撤销命令（{commandRules.length}）</h3>
              <StatusBadge tone="danger" icon={<IconTerminal width={12} height={12} />}>
                不可撤销 · 无逐文件预览
              </StatusBadge>
            </div>
          </div>
          <p className="card-lead">
            这些项调用工具自己的官方命令清理缓存，没有逐文件预览，执行后无法撤销。失败或超时只报告，不会强行终止命令。
          </p>
          <div className="table-wrap">
            <table className="table">
              <thead>
                <tr>
                  <th>名称</th>
                  <th>占盘大小</th>
                  <th>状态</th>
                  <th>说明</th>
                  <th>操作</th>
                </tr>
              </thead>
              <tbody>
                {commandRules.map((rule) => {
                  const candidate = candidates.get(rule.id);
                  const state = commandStates[rule.id];
                  const badge = statusBadge(candidate, rule);
                  const note = statusNote(candidate);
                  const running = state?.phase === "running";
                  // 作用范围要在确认框里写清楚，没解析出缓存路径就不能运行。
                  const paths = candidate?.anchors ?? [];
                  const scope = paths.join("、");

                  return (
                    <Fragment key={rule.id}>
                      <tr>
                        <td className="td-nowrap">{rule.name}</td>
                        <td className="td-num">
                          {candidate ? formatSize(candidate.allocated) : "—"}
                        </td>
                        <td className="td-nowrap">
                          {badge ? (
                            <StatusBadge tone={CANDIDATE_TONES[badge.kind]}>
                              {badge.label}
                            </StatusBadge>
                          ) : (
                            "—"
                          )}
                        </td>
                        <td>
                          <span>{rule.description}</span>
                          {rule.warning && <p className="warn-text">注意：{rule.warning}</p>}
                          {note && <p className="muted" style={{ margin: "3px 0 0" }}>{note}</p>}
                          {paths.length > 0 && (
                            <p className="path-text" style={{ margin: "4px 0 0" }} title={scope}>
                              {scope}
                            </p>
                          )}
                        </td>
                        <td className="td-ops">
                          <div className="actions">
                            <button
                              type="button"
                              className="btn btn-danger-outline"
                              disabled={candidate?.executable !== true || execBusy || scope === ""}
                              onClick={() => runCommand(rule, scope)}
                            >
                              {running ? "运行中…" : "运行"}
                            </button>
                            {renderAskButton(rule.id)}
                          </div>
                        </td>
                      </tr>

                      {running && (
                        <tr className="sub-row">
                          <td colSpan={5}>
                            <p className="muted" style={{ margin: 0 }}>
                              运行中…（超时只报告，不会强行终止）
                            </p>
                          </td>
                        </tr>
                      )}

                      {state?.phase === "done" && (
                        <tr className="sub-row">
                          <td colSpan={5}>{commandResult(state.report)}</td>
                        </tr>
                      )}

                      {state?.phase === "failed" && (
                        <tr className="sub-row">
                          <td colSpan={5}>
                            <p className="danger-text" style={{ margin: 0 }}>
                              执行失败：{state.message}
                            </p>
                          </td>
                        </tr>
                      )}
                      {renderAskSubRow(rule.id, 5)}
                    </Fragment>
                  );
                })}
              </tbody>
            </table>
          </div>
        </div>
      )}

      {plan && (
        <PlanPreview
          // 每份新计划都重建预览：上一次的执行结果、勾选项和展开状态不能带过来，
          // 否则新计划会沿用旧报告的“已结束”状态而没有“开始清理”按钮。
          key={plan.planId}
          planId={plan.planId}
          summary={plan}
          // 命令或清空回收站正在进行时不能开始清理，由后端的执行锁兜底。
          blocked={planning || recycleBusy || anyCommandRunning}
          onRunningChange={setPlanRunning}
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
      // 命令类规则的“找不到工具”就是没安装，文案按任务书写成“未安装”。
      return {
        kind: "notPresent",
        label: rule.targetKind === "command" ? "未安装" : "未找到",
      };
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

// 命令执行结果：成功看前后对比，失败和超时要如实说明，并附上输出末尾。
function commandResult(report: CommandReport) {
  const size = `${formatSize(report.allocatedBefore)} → ${formatSize(report.allocatedAfter)}`;

  return (
    <>
      {report.timedOut ? (
        <p className="warn-text" style={{ margin: 0 }}>
          命令超时，已停止等待；没有强行终止进程，它可能还在运行（缓存 {size}）
        </p>
      ) : report.exitCode === 0 ? (
        <p className="muted td-num" style={{ margin: 0 }}>
          命令已结束：缓存 {size}
        </p>
      ) : (
        <p className="danger-text" style={{ margin: 0 }}>
          命令失败（退出码 {report.exitCode ?? "未知"}）：缓存 {size}
        </p>
      )}
      {report.output && (
        <p className="path-text" style={{ margin: "4px 0 0" }}>
          {report.output}
        </p>
      )}
      {report.logError && <p className="danger-text">{report.logError}</p>}
    </>
  );
}

// 提权执行的结果：等待授权、失败原因，或者每个内置操作的说明。
function elevatedResult(state: ElevatedState) {
  if (state.phase === "running") {
    return (
      <p className="muted" style={{ margin: 0 }}>
        等待管理员授权并执行…
      </p>
    );
  }
  if (state.phase === "failed") {
    return (
      <p className="danger-text" style={{ margin: 0 }}>
        提权执行失败：{state.message}
      </p>
    );
  }

  return (
    <>
      {state.result.ops.map((op) => (
        <Fragment key={op.op}>
          <p className={op.status === "failed" ? "danger-text" : "muted"} style={{ margin: 0 }}>
            {op.detail}
          </p>
          {op.conflicts.length > 0 && (
            <p className="muted" style={{ margin: "4px 0 0" }}>
              原位置已有同名文件、因此没有还原：{op.conflicts.join("、")}
            </p>
          )}
        </Fragment>
      ))}
    </>
  );
}

// 占用检查的结果：占用进程列表，或者“未发现占用 / 检测不完整”。
function lockResult(state: LockState) {
  if (state.phase === "loading") {
    return (
      <p className="muted" style={{ margin: 0 }}>
        正在检查占用…
      </p>
    );
  }
  if (state.phase === "failed") {
    return (
      <p className="danger-text" style={{ margin: 0 }}>
        检测失败：{state.message}
      </p>
    );
  }

  const { report } = state;
  return (
    <>
      {report.lockers.length > 0 ? (
        <ul className="locker-list" style={{ marginTop: 0 }}>
          {report.lockers.map((locker) => (
            <li key={`${locker.pid}-${locker.appName}`}>
              {locker.appName}（PID {locker.pid}）
              {locker.service ? `，服务 ${locker.service}` : ""}
            </li>
          ))}
        </ul>
      ) : (
        <p className="muted" style={{ margin: 0 }}>
          本次未发现占用（检测 {report.checked}/{report.total} 个文件）
        </p>
      )}
      {!report.complete && (
        <p className="warn-text">检测不完整：{report.error ?? "未知原因"}</p>
      )}
    </>
  );
}
