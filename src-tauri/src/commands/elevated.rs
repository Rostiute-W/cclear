//! 提权执行命令：单个需要管理员权限的清理项。
//!
//! 前端只传规则 ID；执行哪个内置操作、系统路径在哪，都由核心库和 helper 决定。

use std::collections::HashSet;
use std::time::SystemTime;

use cclear_core::elevation::{HelperResult, SystemOp, run_elevated};
use cclear_core::rules::catalog::{AdapterId, Rule, Target, find};
use cclear_core::rules::resolve::{RootMap, evaluate};
use cclear_core::win::process::running_processes;
use serde::Serialize;
use tauri::State;

use crate::state::AppState;

/// 一个内置操作的执行结果。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ElevatedOpDto {
    pub op: String,
    /// done / skipped / failed。
    pub status: String,
    pub detail: String,
    pub batch_id: Option<String>,
    pub items: u64,
    /// 还原时原位已有同名文件、因此没有还原的路径。
    pub conflicts: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ElevatedResultDto {
    pub ops: Vec<ElevatedOpDto>,
}

/// 以管理员权限执行一组规则，只弹一次授权。用户取消 UAC 时返回“已取消授权”。
#[tauri::command]
pub async fn elevated_execute(
    rule_ids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<ElevatedResultDto, String> {
    // 提权执行也是清理类操作，和清单、命令共用同一把执行锁。
    let _exec = state.start_exec()?;

    tauri::async_runtime::spawn_blocking(move || {
        if rule_ids.is_empty() {
            return Err("没有要执行的清理规则".to_string());
        }

        // 界面上点按钮时状态可能已经变了，这里按当前状态再确认一次。
        let roots = RootMap::system().map_err(|error| error.to_string())?;
        let procs = running_processes();
        let mut ops = Vec::with_capacity(rule_ids.len());
        // 同一条规则可能被重复选中：helper 里执行两次会做两遍同样的隔离，先去重。
        let mut seen = HashSet::new();

        for rule_id in &rule_ids {
            if !seen.insert(rule_id.as_str()) {
                continue;
            }

            let rule = find(rule_id).ok_or_else(|| "没有这条清理规则".to_string())?;
            if !rule.admin {
                return Err("这条规则不需要管理员权限，请在清理页直接执行".to_string());
            }

            let candidate = evaluate(rule, &roots, &procs, SystemTime::now());
            if !candidate.executable {
                let reason = candidate
                    .status
                    .detail()
                    .unwrap_or_else(|| "请重新评估后再试".to_string());
                return Err(format!("「{}」现在没有执行资格：{reason}", rule.name));
            }

            ops.push(system_op(rule)?);
        }

        let root = crate::commands::quarantine::require_root()?;
        let result = run_elevated(&ops, root.path()).map_err(|error| error.to_string())?;

        Ok(result_dto(result))
    })
    .await
    .map_err(|error| format!("提权清理失败：{error}"))?
}

/// 规则对应的内置操作。M5b 的系统操作还没有实现，不允许从这里发起。
fn system_op(rule: &Rule) -> Result<SystemOp, String> {
    let Target::Adapter(id) = &rule.target else {
        return Err("这条规则不是提权 helper 的内置操作".to_string());
    };

    match id {
        AdapterId::LeAppStoreCicada => Ok(SystemOp::LeAppStoreCicada),
        AdapterId::NvidiaAppOtaArtifacts => Ok(SystemOp::NvidiaOta),
        _ => Err(format!("「{}」没有对应的提权操作", rule.name)),
    }
}

/// 结果原样转成 IPC 结构。系统维护命令也复用它。
pub(crate) fn result_dto(result: HelperResult) -> ElevatedResultDto {
    ElevatedResultDto {
        ops: result
            .ops
            .into_iter()
            .map(|op| ElevatedOpDto {
                op: op.op,
                status: op.status,
                detail: op.detail,
                batch_id: op.batch_id,
                items: op.items,
                conflicts: op.conflicts,
            })
            .collect(),
    }
}
