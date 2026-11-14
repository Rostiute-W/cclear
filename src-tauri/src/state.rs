//! 跨命令共享的运行时状态。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cclear_core::plan::FrozenPlan;
use cclear_core::scan::ScanResult;
use uuid::Uuid;

/// 最近一次扫描结果、当前扫描的取消开关，以及冻结清单。
///
/// 执行类命令只接受节点 ID 或文件序号，路径一律从这里取，前端拿不到自由路径。
#[derive(Default)]
pub struct AppState {
    scan_counter: AtomicU64,
    /// 是否有扫描正在后台运行。
    running: AtomicBool,
    root: Mutex<Option<String>>,
    result: Mutex<Option<Arc<ScanResult>>>,
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// 冻结清单只保留最近一份：新预览会替换旧清单，避免执行一份已经过期的计划。
    plans: Mutex<HashMap<Uuid, Arc<FrozenPlan>>>,
    /// 是否有清理正在执行。同一时间只允许一次，否则两份清单会互相干扰。
    exec_running: AtomicBool,
    exec_cancel: Mutex<Option<Arc<AtomicBool>>>,
}

impl AppState {
    /// 登记一次新扫描，返回扫描 ID 和取消开关。
    ///
    /// 同一时间只允许一次扫描：两次扫描会互相覆盖结果和取消标志。
    pub fn start_scan(&self) -> Result<(String, Arc<AtomicBool>), String> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Err("已经有扫描在进行，请等待结束或先取消".to_string());
        }

        let cancel = Arc::new(AtomicBool::new(false));
        *self.cancel.lock().unwrap() = Some(cancel.clone());

        let index = self.scan_counter.fetch_add(1, Ordering::Relaxed) + 1;
        Ok((format!("scan-{index}"), cancel))
    }

    /// 保存扫描结果并清掉运行标记。
    pub fn finish_scan(&self, result: ScanResult, root: String) {
        *self.root.lock().unwrap() = Some(root);
        *self.result.lock().unwrap() = Some(Arc::new(result));
        self.clear_running();
    }

    /// 扫描失败或线程异常时只清掉运行标记，不动上一次的结果。
    pub fn abort_scan(&self) {
        self.clear_running();
    }

    /// 请求取消当前扫描。没有扫描时什么也不做。
    pub fn cancel_scan(&self) {
        if let Some(flag) = self.cancel.lock().unwrap().as_ref() {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// 取最近一次扫描结果。还没扫过时返回 None。
    pub fn result(&self) -> Option<Arc<ScanResult>> {
        self.result.lock().unwrap().clone()
    }

    /// 最近一次扫描的根路径。扫描刚起步就被取消时结果里没有根节点，界面靠它显示扫描范围。
    pub fn scan_root(&self) -> String {
        self.root.lock().unwrap().clone().unwrap_or_default()
    }

    fn clear_running(&self) {
        *self.cancel.lock().unwrap() = None;
        self.running.store(false, Ordering::SeqCst);
    }

    /// 存下一份新的冻结清单，并丢掉之前那份。
    pub fn store_plan(&self, plan: FrozenPlan) {
        let plan_id = plan.plan_id;
        let mut plans = self.plans.lock().unwrap();
        plans.clear();
        plans.insert(plan_id, Arc::new(plan));
    }

    /// 按 ID 取冻结清单。过期或被替换时返回 None。
    pub fn plan(&self, plan_id: Uuid) -> Option<Arc<FrozenPlan>> {
        self.plans.lock().unwrap().get(&plan_id).cloned()
    }

    /// 登记一次清理执行，返回取消开关。
    pub fn start_exec(&self) -> Result<Arc<AtomicBool>, String> {
        if self.exec_running.swap(true, Ordering::SeqCst) {
            return Err("已经有一次清理在进行，请等待结束或先取消".to_string());
        }

        let cancel = Arc::new(AtomicBool::new(false));
        *self.exec_cancel.lock().unwrap() = Some(cancel.clone());

        Ok(cancel)
    }

    /// 执行结束（无论成功、中止还是取消）后清掉运行标记。
    pub fn finish_exec(&self) {
        *self.exec_cancel.lock().unwrap() = None;
        self.exec_running.store(false, Ordering::SeqCst);
    }

    /// 请求取消当前清理。没有清理时什么也不做。
    pub fn cancel_exec(&self) {
        if let Some(flag) = self.exec_cancel.lock().unwrap().as_ref() {
            flag.store(true, Ordering::SeqCst);
        }
    }
}
