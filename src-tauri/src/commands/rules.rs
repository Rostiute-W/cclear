//! 规则目录与候选评估命令。只读，不涉及任何执行。

use std::time::SystemTime;

use cclear_core::model::{Category, Phase, Recovery, Tier};
use cclear_core::rules::catalog::{Rule, Target, catalog};
use cclear_core::rules::commands;
use cclear_core::rules::resolve::{Candidate, RootMap, evaluate_all};
use cclear_core::win::process::running_processes;
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleDto {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: Category,
    pub tier: Tier,
    pub recovery: Recovery,
    pub phase: Phase,
    pub admin: bool,
    pub default_selected: bool,
    pub processes: Vec<String>,
    pub warning: Option<String>,
    /// 目标类型：界面对命令类和特殊操作显示不同的引导文案。
    pub target_kind: String,
    /// 命令类规则调用的工具和二次确认要求，界面据此写确认文案。
    pub command: Option<CommandDto>,
    /// 系统操作规则对应的内置操作名。没有内置操作时为 None，
    /// 界面据此把还没实现的项排除在“系统维护”分组之外。
    pub system_op: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandDto {
    /// 调用的工具名，例如 npm。
    pub tool: String,
    /// 需要在普通确认之后再确认一次。
    pub needs_second_confirm: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateDto {
    pub rule_id: String,
    /// 机器可读的状态名，界面据此选徽标。
    pub status: String,
    pub status_detail: Option<String>,
    pub anchors: Vec<String>,
    pub logical: u64,
    pub allocated: u64,
    pub files: u64,
    pub samples: Vec<String>,
    /// 判定依据，一条一个条目。残留规则靠它向用户说明为什么选中这些目录。
    pub evidence: Vec<String>,
    /// 没有选中某些项的原因。
    pub notes: Vec<String>,
    pub executable: bool,
}

#[tauri::command]
pub fn rules_catalog() -> Vec<RuleDto> {
    catalog().iter().map(rule_dto).collect()
}

#[tauri::command]
pub async fn rules_evaluate() -> Result<Vec<CandidateDto>, String> {
    // 评估要扫磁盘，放到阻塞线程池里执行，避免占住异步运行时。
    tauri::async_runtime::spawn_blocking(|| {
        let roots = RootMap::system().map_err(|error| error.to_string())?;
        let procs = running_processes();
        let now = SystemTime::now();

        // 规则之间互不依赖，跨规则并行评估；返回值顺序与规则目录一致。
        Ok(evaluate_all(&roots, &procs, now)
            .into_iter()
            .map(candidate_dto)
            .collect())
    })
    .await
    .map_err(|error| format!("规则评估失败：{error}"))?
}

fn rule_dto(rule: &Rule) -> RuleDto {
    RuleDto {
        id: rule.id.to_string(),
        name: rule.name.to_string(),
        description: rule.description.to_string(),
        category: rule.category,
        tier: rule.tier,
        recovery: rule.recovery,
        phase: rule.phase,
        admin: rule.admin,
        default_selected: rule.default_selected,
        processes: rule.processes.iter().map(|name| name.to_string()).collect(),
        warning: rule.warning.map(|text| text.to_string()),
        target_kind: target_kind(&rule.target).to_string(),
        command: command_dto(&rule.target),
        system_op: system_op(&rule.target),
    }
}

/// 系统操作规则对应的内置操作名。其他目标没有内置操作，返回 None。
fn system_op(target: &Target) -> Option<String> {
    let Target::System(id) = target else {
        return None;
    };

    cclear_core::elevation::SystemOp::of_id(*id).map(|op| op.token())
}

/// 命令类规则的工具信息。其他目标没有命令，返回 None。
fn command_dto(target: &Target) -> Option<CommandDto> {
    let Target::Command(id) = target else {
        return None;
    };
    let spec = commands::spec(*id)?;

    Some(CommandDto {
        tool: spec.tool.to_string(),
        needs_second_confirm: spec.needs_second_confirm,
    })
}

fn candidate_dto(candidate: Candidate) -> CandidateDto {
    CandidateDto {
        rule_id: candidate.rule_id.to_string(),
        status: candidate.status.kind().to_string(),
        status_detail: candidate.status.detail(),
        anchors: candidate
            .anchors
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        logical: candidate.size.logical,
        allocated: candidate.size.allocated,
        files: candidate.size.files,
        samples: candidate
            .samples
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        evidence: candidate.evidence,
        notes: candidate.notes,
        executable: candidate.executable,
    }
}

fn target_kind(target: &Target) -> &'static str {
    match target {
        Target::Paths { .. } => "paths",
        Target::ChromiumProfiles { .. } => "chromiumProfiles",
        Target::Adapter(_) => "adapter",
        Target::Command(_) => "command",
        Target::Special(_) => "special",
        Target::System(_) => "system",
    }
}
