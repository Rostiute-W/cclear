use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use cclear_core::orchestrator::{JunkScan, deep_slim::WinSxSState, space::SpaceScan};

#[derive(Default)]
pub struct AppState {
    pub operation: Mutex<()>,
    pub junk: Mutex<Option<JunkScan>>,
    pub space: Mutex<Option<Arc<SpaceScan>>>,
    pub conflicts: Mutex<Vec<String>>,
    pub winsxs: Mutex<WinSxSState>,
    pub junk_cancel: Cancellation,
    pub clean_cancel: Cancellation,
    pub space_cancel: Cancellation,
}

pub fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, String> {
    mutex
        .lock()
        .map_err(|_| "应用状态不可用，请重启 CClear".into())
}

#[derive(Default)]
pub struct Cancellation(Mutex<Option<Arc<AtomicBool>>>);

pub struct Active<'a> {
    owner: &'a Cancellation,
    pub flag: Arc<AtomicBool>,
}

impl Cancellation {
    pub fn start(&self) -> Result<Active<'_>, String> {
        let mut current = lock(&self.0)?;
        if current.is_some() {
            return Err("已有同类操作正在运行".into());
        }
        let flag = Arc::new(AtomicBool::new(false));
        *current = Some(Arc::clone(&flag));
        Ok(Active { owner: self, flag })
    }

    pub fn cancel(&self) -> Result<(), String> {
        if let Some(flag) = lock(&self.0)?.as_ref() {
            flag.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
}

impl Drop for Active<'_> {
    fn drop(&mut self) {
        if let Ok(mut current) = self.owner.0.lock() {
            *current = None;
        }
    }
}
