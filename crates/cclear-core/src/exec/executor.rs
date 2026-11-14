//! 按冻结清单执行清理：只动清单里的条目，每项都过一遍路径护栏。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::guard::{self, DirHandle, Expect, Forbidden, GuardError, OpenIntent};
use crate::journal::{JournalSink, Record, unix_now};
use crate::model::Recovery;
use crate::plan::{FrozenItem, FrozenPlan, PlanRule};
use crate::quarantine::{BatchStage, FrozenExpect, QuarantineItem, QuarantineRoot};
use crate::rules::catalog::find;
use crate::win::process::ProcInfo;
use crate::win::volume;

/// 报告里最多保留多少条跳过明细，避免界面被几万条记录撑爆。
const SKIP_DETAIL_LIMIT: usize = 200;

/// 日志写不进去时的固定文案：宁可停下，也不能留下没有记录的删除。
const LOG_ABORT: &str = "日志写入失败，已停止";

/// 推给界面的执行进度。
pub struct ExecProgress {
    pub rule_id: String,
    pub deleted_files: u64,
    pub deleted_dirs: u64,
    pub freed_allocated: u64,
    pub skipped: u64,
    /// 执行已结束，界面据此从进度切换到结果。
    pub done: bool,
}

/// 一条规则的执行结果。
pub struct RuleReport {
    pub rule_id: &'static str,
    pub deleted_files: u64,
    pub deleted_dirs: u64,
    pub freed_allocated: u64,
    pub skipped: u64,
}

/// 一条被跳过的条目。
pub struct SkippedItem {
    pub rule_id: &'static str,
    pub path: String,
    pub reason: String,
}

/// 一次执行的结果。
pub struct ExecReport {
    pub rules: Vec<RuleReport>,
    /// 跳过明细，最多 `SKIP_DETAIL_LIMIT` 条。
    pub skipped: Vec<SkippedItem>,
    /// 跳过总数，可能大于明细条数。
    pub skipped_total: u64,
    pub free_before: u64,
    pub free_after: u64,
    pub cancelled: bool,
    /// 中止原因。中止后不再处理任何条目。
    pub aborted: Option<String>,
}

impl ExecReport {
    fn add_skip(&mut self, rule_id: &'static str, path: String, reason: String) {
        self.skipped_total += 1;
        if self.skipped.len() < SKIP_DETAIL_LIMIT {
            self.skipped.push(SkippedItem {
                rule_id,
                path,
                reason,
            });
        }
    }

    fn total_deleted_files(&self) -> u64 {
        self.rules.iter().map(|rule| rule.deleted_files).sum()
    }

    fn total_deleted_dirs(&self) -> u64 {
        self.rules.iter().map(|rule| rule.deleted_dirs).sum()
    }

    fn total_freed_allocated(&self) -> u64 {
        self.rules.iter().map(|rule| rule.freed_allocated).sum()
    }
}

/// 规则之间为什么停下。
enum Stop {
    /// 还有规则要处理。
    Continue,
    /// 日志写失败，不能继续删。
    Aborted,
    /// 用户取消。
    Cancelled,
}

/// 执行期间各规则共用的上下文。
struct ExecContext<'a> {
    plan: &'a FrozenPlan,
    forbidden: &'a Forbidden,
    procs_now: &'a dyn Fn() -> Vec<ProcInfo>,
    journal: &'a mut dyn JournalSink,
    cancel: &'a AtomicBool,
    progress: &'a dyn Fn(ExecProgress),
}

/// 按冻结清单执行。清单之外的路径一律不碰：条目只能按锚点 + 相对组件打开。
pub fn execute(
    plan: &FrozenPlan,
    selected_rule_ids: &[&str],
    forbidden: &Forbidden,
    procs_now: &dyn Fn() -> Vec<ProcInfo>,
    journal: &mut dyn JournalSink,
    cancel: &AtomicBool,
    progress: &dyn Fn(ExecProgress),
) -> ExecReport {
    let selected: HashSet<&str> = selected_rule_ids.iter().copied().collect();
    let mut report = ExecReport {
        rules: Vec::new(),
        skipped: Vec::new(),
        skipped_total: 0,
        free_before: plan_free_space(plan),
        free_after: 0,
        cancelled: false,
        aborted: None,
    };

    let started = Record::PlanStarted {
        plan_id: plan.plan_id,
        at: unix_now(),
        rules: plan
            .rules
            .iter()
            .filter(|rule| selected.contains(rule.rule_id))
            .map(|rule| rule.rule_id.to_string())
            .collect(),
    };
    if journal.append(&started).is_err() {
        report.aborted = Some(LOG_ABORT.to_string());
        report.free_after = plan_free_space(plan);
        return report;
    }

    let mut context = ExecContext {
        plan,
        forbidden,
        procs_now,
        journal,
        cancel,
        progress,
    };

    for plan_rule in &plan.rules {
        if !selected.contains(plan_rule.rule_id) {
            continue;
        }

        let (rule_report, stop) = context.run_rule(plan_rule, &mut report);
        report.rules.push(rule_report);

        match stop {
            Stop::Continue => {}
            Stop::Aborted => {
                report.aborted = Some(LOG_ABORT.to_string());
                break;
            }
            Stop::Cancelled => {
                report.cancelled = true;
                break;
            }
        }
    }

    // 中止也要记下执行后的空间，报告里才看得出磁盘变化。
    report.free_after = plan_free_space(plan);

    if report.aborted.is_none() {
        let finished = Record::PlanFinished {
            plan_id: plan.plan_id,
            free_before: report.free_before,
            free_after: report.free_after,
            deleted_files: report.total_deleted_files(),
            deleted_dirs: report.total_deleted_dirs(),
            freed_allocated: report.total_freed_allocated(),
            skipped: report.skipped_total,
            // 取消的执行在历史里要显示成“已取消”，不能当成正常结束。
            cancelled: report.cancelled,
        };
        if journal.append(&finished).is_err() || journal.finish().is_err() {
            report.aborted = Some(LOG_ABORT.to_string());
        }
    }

    progress(ExecProgress {
        rule_id: String::new(),
        deleted_files: report.total_deleted_files(),
        deleted_dirs: report.total_deleted_dirs(),
        freed_allocated: report.total_freed_allocated(),
        skipped: report.skipped_total,
        done: true,
    });

    report
}

impl ExecContext<'_> {
    /// 执行一条规则，返回这条规则的统计和停止原因。
    fn run_rule(&mut self, plan_rule: &PlanRule, report: &mut ExecReport) -> (RuleReport, Stop) {
        let rule_id = plan_rule.rule_id;
        let mut rule_report = RuleReport {
            rule_id,
            deleted_files: 0,
            deleted_dirs: 0,
            freed_allocated: 0,
            skipped: 0,
        };

        // 规则开始前重新查一次进程：预览之后应用可能又被打开了。
        let running = running_required_processes(plan_rule, (self.procs_now)());
        if !running.is_empty() {
            let reason = format!("相关程序正在运行：{}", running.join("、"));
            if !self.skip_rule(rule_id, &reason) {
                return (rule_report, Stop::Aborted);
            }

            rule_report.skipped += 1;
            report.add_skip(rule_id, String::new(), reason);
            return (rule_report, Stop::Continue);
        }

        // 隔离类规则不走逐项删除，整条交给隔离引擎处理。
        if plan_rule.recovery == Recovery::Quarantine {
            return self.quarantine_rule(plan_rule, rule_report, report);
        }

        let items = &self.plan.items[plan_rule.items.start as usize..plan_rule.items.end as usize];
        let mut at = 0usize;

        while at < items.len() {
            // 清单是按锚点顺序生成的，同一锚点的条目连续出现。
            let anchor_index = items[at].anchor_index as usize;
            let start = at;
            while at < items.len() && items[at].anchor_index as usize == anchor_index {
                at += 1;
            }
            let group = &items[start..at];

            if self.cancel.load(Ordering::SeqCst) {
                return (rule_report, Stop::Cancelled);
            }

            let anchor_path = &plan_rule.anchors[anchor_index];
            let anchor = match guard::open_anchor(anchor_path, self.forbidden) {
                Ok(anchor) => anchor,
                Err(error) => {
                    // 锚点打不开就整组跳过，不退回字符串路径去删。
                    let reason = error.to_string();
                    if !self.skip_rule(rule_id, &reason) {
                        return (rule_report, Stop::Aborted);
                    }

                    rule_report.skipped += group.len() as u64;
                    report.add_skip(rule_id, anchor_path.to_string_lossy().into_owned(), reason);
                    continue;
                }
            };

            for item in group {
                if self.cancel.load(Ordering::SeqCst) {
                    return (rule_report, Stop::Cancelled);
                }

                let stop = self.run_item(
                    &anchor,
                    anchor_path,
                    plan_rule,
                    item,
                    &mut rule_report,
                    report,
                );
                if let Some(stop) = stop {
                    return (rule_report, stop);
                }

                (self.progress)(ExecProgress {
                    rule_id: rule_id.to_string(),
                    deleted_files: rule_report.deleted_files,
                    deleted_dirs: rule_report.deleted_dirs,
                    freed_allocated: rule_report.freed_allocated,
                    skipped: rule_report.skipped,
                    done: false,
                });
            }
        }

        (rule_report, Stop::Continue)
    }

    /// 处理一个条目。返回 `Some` 表示这条规则要停下。
    fn run_item(
        &mut self,
        anchor: &DirHandle,
        anchor_path: &Path,
        plan_rule: &PlanRule,
        item: &FrozenItem,
        rule_report: &mut RuleReport,
        report: &mut ExecReport,
    ) -> Option<Stop> {
        let rule_id = plan_rule.rule_id;
        let path = display_path(anchor_path, item);

        match delete_item(anchor, item, self.forbidden) {
            Ok(()) => {
                if item.is_dir {
                    rule_report.deleted_dirs += 1;
                } else {
                    rule_report.deleted_files += 1;
                }
                rule_report.freed_allocated += item.allocated;

                let done = Record::ItemDone {
                    plan_id: self.plan.plan_id,
                    item_id: item.item_id,
                    rule_id: rule_id.to_string(),
                    path,
                    is_dir: item.is_dir,
                    allocated: item.allocated,
                };
                // 每处理完一个条目就写日志；写不进去必须立刻停下。
                if self.journal.append(&done).is_err() {
                    return Some(Stop::Aborted);
                }
            }
            Err(error) => {
                let reason = skip_reason(&error);
                rule_report.skipped += 1;
                report.add_skip(rule_id, path.clone(), reason.clone());

                let skipped = Record::ItemSkipped {
                    plan_id: self.plan.plan_id,
                    item_id: item.item_id,
                    rule_id: rule_id.to_string(),
                    path,
                    reason,
                };
                if self.journal.append(&skipped).is_err() {
                    return Some(Stop::Aborted);
                }
            }
        }

        None
    }

    /// 隔离类规则：整条交给隔离引擎，复制校验通过后才删源。
    ///
    /// 目录条目本身不复制，文件都隔离之后留下的空目录由用户自己处理。
    fn quarantine_rule(
        &mut self,
        plan_rule: &PlanRule,
        mut rule_report: RuleReport,
        report: &mut ExecReport,
    ) -> (RuleReport, Stop) {
        let rule_id = plan_rule.rule_id;

        let mut items: Vec<QuarantineItem> = Vec::new();
        let frozen = &self.plan.items[plan_rule.items.start as usize..plan_rule.items.end as usize];
        for item in frozen {
            let anchor = plan_rule
                .anchors
                .get(item.anchor_index as usize)
                .map(PathBuf::as_path)
                .unwrap_or_else(|| Path::new(""));
            let path = display_path(anchor, item);

            if item.is_dir {
                rule_report.skipped += 1;
                report.add_skip(rule_id, path, "目录不隔离，已跳过".to_string());
                continue;
            }

            // 冻结清单里的身份、大小和修改时间交给隔离引擎，预检时逐项复核。
            items.push(QuarantineItem {
                anchor_index: item.anchor_index,
                rel: item.rel.clone(),
                frozen: Some(FrozenExpect {
                    identity: item.identity,
                    size: item.size,
                    mtime: item.mtime,
                }),
            });
        }

        if items.is_empty() {
            return (rule_report, Stop::Continue);
        }

        // 没有设置隔离区位置时不能退回直接删除，只能整条跳过。
        let Some(root) = QuarantineRoot::from_settings() else {
            let reason = "还没有设置隔离区位置，请在隔离区页选择保存卷".to_string();
            if !self.skip_rule(rule_id, &reason) {
                return (rule_report, Stop::Aborted);
            }

            rule_report.skipped += items.len() as u64;
            report.add_skip(rule_id, String::new(), reason);
            return (rule_report, Stop::Continue);
        };

        let outcome = root.quarantine(
            rule_id,
            self.forbidden,
            &plan_rule.anchors,
            &items,
            self.journal,
            self.cancel,
        );

        match outcome {
            Ok(outcome) => {
                rule_report.deleted_files += outcome.moved;
                rule_report.freed_allocated += outcome.moved_size;
                rule_report.skipped += outcome.kept;

                for (path, reason) in outcome.skipped {
                    rule_report.skipped += 1;
                    report.add_skip(rule_id, path.to_string_lossy().into_owned(), reason);
                }

                // 批次没有走到“已隔离”时把原因带回执行报告。
                if outcome.stage != BatchStage::Committed {
                    report.add_skip(
                        rule_id,
                        String::new(),
                        format!("隔离批次 {}：{}", outcome.batch_id, outcome.stage.label()),
                    );
                }
            }
            Err(error) => {
                let reason = error.to_string();
                if !self.skip_rule(rule_id, &reason) {
                    return (rule_report, Stop::Aborted);
                }

                rule_report.skipped += items.len() as u64;
                report.add_skip(rule_id, String::new(), reason);
            }
        }

        (rule_report, Stop::Continue)
    }

    /// 整条规则跳过时写日志，返回是否写成功。
    fn skip_rule(&mut self, rule_id: &'static str, reason: &str) -> bool {
        let record = Record::RuleSkipped {
            plan_id: self.plan.plan_id,
            rule_id: rule_id.to_string(),
            reason: reason.to_string(),
        };

        self.journal.append(&record).is_ok()
    }
}

/// 清单一律按“锚点 + 相对组件”打开，删完在同一次打开得到的条目上复核。
fn delete_item(
    anchor: &DirHandle,
    item: &FrozenItem,
    forbidden: &Forbidden,
) -> Result<(), GuardError> {
    let entry = guard::open_rel(anchor, &item.rel, OpenIntent::Delete, forbidden)?;
    let expect = Expect {
        identity: item.identity,
        size: item.size,
        mtime: item.mtime,
    };

    guard::delete(entry, &expect)
}

/// 规则要求退出的进程里，当前正在运行的那些。
///
/// 进程名优先取清单里带过来的（迁移清理这类临时规则不在规则目录里），
/// 清单没带时再按规则 ID 查规则目录。
fn running_required_processes(plan_rule: &PlanRule, procs: Vec<ProcInfo>) -> Vec<String> {
    let required: Vec<String> = if plan_rule.processes.is_empty() {
        find(plan_rule.rule_id)
            .map(|rule| rule.processes.iter().map(|name| name.to_string()).collect())
            .unwrap_or_default()
    } else {
        plan_rule
            .processes
            .iter()
            .map(|name| name.to_string())
            .collect()
    };

    required
        .into_iter()
        .filter(|required| {
            procs
                .iter()
                .any(|proc| crate::win::equals_ignore_case(&proc.name, required))
        })
        .collect()
}

/// 护栏错误到中文原因。
fn skip_reason(error: &GuardError) -> String {
    match error {
        GuardError::Changed => "已变化".to_string(),
        GuardError::Gone => "已不存在".to_string(),
        GuardError::Locked => "被占用".to_string(),
        GuardError::Denied => "无权限".to_string(),
        GuardError::ReadOnly => "只读".to_string(),
        GuardError::DirNotEmpty => "目录内有未计划的新文件已保留".to_string(),
        GuardError::ReparseInPath(_) => "路径中出现链接".to_string(),
        // 其余错误自身的显示文本已经是中文原因。
        _ => error.to_string(),
    }
}

/// 条目在界面上的完整路径，只用于展示和日志。
fn display_path(anchor: &Path, item: &FrozenItem) -> String {
    let mut path = anchor.to_path_buf();
    for part in &item.rel {
        path.push(part);
    }

    path.to_string_lossy().into_owned()
}

/// 锚点所在卷的剩余空间。所有规则的锚点都在系统盘，取第一个锚点代表整份清单。
fn plan_free_space(plan: &FrozenPlan) -> u64 {
    plan.rules
        .iter()
        .find_map(|rule| rule.anchors.first())
        .map(|anchor| volume::free_space(anchor))
        .unwrap_or(0)
}
