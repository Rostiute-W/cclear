// 隔离区页：选择保存卷，查看批次，按批次还原或彻底删除。
import { useEffect, useState } from "react";
import {
  quarantineBatches,
  quarantinePurge,
  quarantineResolve,
  quarantineRestore,
  quarantineSetVolume,
  quarantineVolumes,
} from "../api";
import { IconQuarantine } from "../components/Icons";
import { AdminTag, AlertBanner, EmptyState, StatusBadge, type SemanticTone } from "../components/Ui";
import { formatSize, formatTime } from "../format";
import type { QuarantineBatch, QuarantineStage, QuarantineVolumes } from "../types";

// 超过这个天数的批次在页面上单独提醒。
const OVERDUE_DAYS = 7;

const STAGE_TONES: Record<QuarantineStage, SemanticTone> = {
  planned: "neutral",
  copying: "info",
  copiedVerified: "info",
  deletingSource: "caution",
  committed: "safe",
  restoring: "info",
  restored: "safe",
  purging: "danger",
  purged: "neutral",
  aborted: "neutral",
  needsAttention: "caution",
};

export function QuarantinePage({ onChanged }: { onChanged: () => void }) {
  const [volumes, setVolumes] = useState<QuarantineVolumes | null>(null);
  const [batches, setBatches] = useState<QuarantineBatch[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const reload = () => {
    Promise.all([quarantineVolumes(), quarantineBatches()])
      .then(([volumeState, list]) => {
        setVolumes(volumeState);
        setBatches(list);
      })
      .catch((reason) => setError(String(reason)));
  };

  useEffect(reload, []);

  // 所有会动文件的动作共用一次执行状态：执行期间禁用其他按钮，避免同时操作同一批文件。
  const run = async (action: () => Promise<string>) => {
    setBusy(true);
    setError(null);
    setMessage(null);

    try {
      setMessage(await action());
      reload();
      // 批次变了，顶部的超期提示也要重新算一次。
      onChanged();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const onSelectVolume = (root: string) => {
    void run(async () => {
      const updated = await quarantineSetVolume(root);
      setVolumes(updated);
      return "隔离区位置已保存";
    });
  };

  const onRestore = (batch: QuarantineBatch) => {
    void run(async () => {
      const result = await quarantineRestore(batch.batchId);
      const base = `已还原 ${result.restored} 个文件（${result.stageLabel}）`;
      if (result.conflicts.length === 0) {
        return base;
      }
      return `${base}，另有 ${result.conflicts.length} 个因原位已有同名文件未还原`;
    });
  };

  const onPurge = (batch: QuarantineBatch) => {
    const confirmed = window.confirm(
      `彻底删除这个批次？批次里的 ${batch.items} 个文件将无法恢复。`,
    );
    if (!confirmed) {
      return;
    }

    void run(async () => {
      await quarantinePurge(batch.batchId);
      return "批次已彻底删除";
    });
  };

  const onResolve = (batch: QuarantineBatch, action: "continue" | "abandon") => {
    if (
      action === "abandon" &&
      !window.confirm(
        "放弃这个批次？只会删除源文件仍在原位且未变化的副本；源已不在的条目会保留在隔离区。",
      )
    ) {
      return;
    }

    void run(async () => {
      const result = await quarantineResolve(batch.batchId, action);
      const prefix = action === "continue" ? "已继续处理" : "已放弃副本，源文件保持不变";
      return `${prefix}：隔离 ${result.moved} 个，源保留 ${result.kept} 个（${result.stageLabel}）`;
    });
  };

  return (
    <section className="page-section">
      <div className="page-header">
        <div>
          <h2 className="page-title">隔离区</h2>
          <p className="page-subtitle">残留清理的条目先存于非系统盘隔离区，可按批次还原或彻底清除</p>
        </div>
      </div>

      {error && <AlertBanner tone="danger">{error}</AlertBanner>}
      {message && <AlertBanner tone="success">{message}</AlertBanner>}

      <div className="card">
        <div className="card-header">
          <h3 className="card-title">保存位置</h3>
          {volumes?.currentRoot && (
            <StatusBadge tone="safe">
              当前隔离区：{volumes.currentRoot}（剩余 {formatSize(volumes.currentFree)}）
            </StatusBadge>
          )}
        </div>

        {!volumes && <p className="muted">正在读取可用卷…</p>}

        {volumes && (
          <>
            <div className="field-row">
              <span className="field-label">隔离区卷</span>
              <select
                className="select-input"
                value={currentVolume(volumes)}
                disabled={busy || volumes.candidates.length === 0}
                onChange={(event) => onSelectVolume(event.target.value)}
              >
                {volumes.candidates.map((volume) => (
                  <option key={volume.root} value={volume.root}>
                    {volume.root} {volume.label}（剩余 {formatSize(volume.free)}）
                  </option>
                ))}
              </select>
            </div>

            {volumes.candidates.length === 0 && (
              <div style={{ marginTop: 12 }}>
                <AlertBanner tone="warn">
                  没有可用的卷：隔离区不能用系统盘，需要一块格式化为 NTFS 的本地固定盘。
                </AlertBanner>
              </div>
            )}

            {volumes.currentRoot ? (
              <p className="muted" style={{ marginTop: 10, marginBottom: 0 }}>
                当前隔离区：{volumes.currentRoot}（剩余 {formatSize(volumes.currentFree)}）
              </p>
            ) : (
              <div style={{ marginTop: 12 }}>
                <AlertBanner tone="warn">还没有设置隔离区位置，隔离类清理会被跳过。</AlertBanner>
              </div>
            )}
          </>
        )}
      </div>

      <div className="card">
        <div className="card-header">
          <h3 className="card-title">批次</h3>
          <span className="muted td-num">共 {batches.length} 个批次</span>
        </div>

        {batches.length === 0 ? (
          <EmptyState
            icon={<IconQuarantine />}
            title="隔离区里还没有批次"
            description="在「清理」页将残留项移入隔离区后，可在这里按批次还原或彻底删除。"
          />
        ) : (
          <>
            {batches.some((batch) => batch.ageDays > OVERDUE_DAYS) && (
              <div style={{ marginBottom: 12 }}>
                <AlertBanner tone="warn">
                  有批次存放超过 {OVERDUE_DAYS} 天，需要你决定还原还是彻底删除。
                </AlertBanner>
              </div>
            )}

            <div className="table-wrap">
              <table className="table">
                <thead>
                  <tr>
                    <th>时间</th>
                    <th>来源规则</th>
                    <th>条目</th>
                    <th>大小</th>
                    <th>状态</th>
                    <th>存放天数</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  {batches.map((batch) => (
                    <tr key={batch.batchId}>
                      <td className="td-num">{formatTime(batch.createdAt)}</td>
                      <td>{batch.ruleName}</td>
                      <td className="td-num">{batch.items.toLocaleString()}</td>
                      <td className="td-num">{formatSize(batch.size)}</td>
                      <td>
                        <div className="actions">
                          <StatusBadge tone={STAGE_TONES[batch.stage]}>
                            {batch.stageLabel}
                          </StatusBadge>
                          {batch.requiresAdmin && <AdminTag />}
                        </div>
                        {batch.note && (
                          <div className="muted" style={{ marginTop: 4 }}>
                            {batch.note}
                          </div>
                        )}
                      </td>
                      <td className="td-num">
                        {batch.ageDays > OVERDUE_DAYS ? (
                          <StatusBadge tone="caution">{batch.ageDays} 天</StatusBadge>
                        ) : (
                          batch.ageDays
                        )}
                      </td>
                      <td>
                        <div className="actions">
                          <button
                            type="button"
                            className="btn btn-primary"
                            disabled={busy || !canRestore(batch)}
                            onClick={() => onRestore(batch)}
                          >
                            还原
                          </button>
                          <button
                            type="button"
                            className="btn btn-danger-outline"
                            disabled={busy || !canPurge(batch)}
                            onClick={() => onPurge(batch)}
                          >
                            彻底删除
                          </button>
                          {batch.stage === "needsAttention" && (
                            <>
                              <button
                                type="button"
                                className="btn btn-subtle"
                                disabled={busy}
                                onClick={() => onResolve(batch, "continue")}
                              >
                                处理异常：继续
                              </button>
                              <button
                                type="button"
                                className="btn btn-subtle"
                                disabled={busy}
                                onClick={() => onResolve(batch, "abandon")}
                              >
                                处理异常：放弃
                              </button>
                            </>
                          )}
                        </div>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </>
        )}
      </div>
    </section>
  );
}

// 当前隔离根所在的卷。还没设置时返回空串，让下拉框自然显示第一项。
function currentVolume(volumes: QuarantineVolumes): string {
  if (!volumes.currentRoot) {
    return "";
  }

  const matched = volumes.candidates.find((volume) =>
    volumes.currentRoot?.startsWith(volume.root),
  );
  return matched?.root ?? "";
}

// 只有已经隔离完成的批次才有文件需要还原。
function canRestore(batch: QuarantineBatch): boolean {
  return batch.stage === "committed" || batch.stage === "needsAttention";
}

// 正在读写文件的批次不能直接删除。
function canPurge(batch: QuarantineBatch): boolean {
  return !["copying", "restoring", "purging"].includes(batch.stage);
}
