use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::E_ABORT;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows::Win32::UI::Shell::{
    COPYENGINE_E_RECYCLE_BIN_NOT_FOUND, COPYENGINE_E_RECYCLE_FORCE_NUKE,
    COPYENGINE_E_RECYCLE_PATH_TOO_LONG, COPYENGINE_E_RECYCLE_SIZE_TOO_BIG,
    COPYENGINE_E_RECYCLE_UNKNOWN_ERROR, FOF_ALLOWUNDO, FOF_NOERRORUI, FOF_SILENT,
    FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE, FileOperation, IFileOperation,
    IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem,
    SHCreateItemFromParsingName, TSF_DELETE_RECYCLE_IF_POSSIBLE,
};
use windows::core::{ComObject, HRESULT, PCWSTR, Ref, Result as WinResult};

use super::{Forbidden, GuardError, path};

#[windows::core::implement(IFileOperationProgressSink)]
struct RecycleSink {
    cannot_recycle: AtomicBool,
}

fn is_recycle_error(status: HRESULT) -> bool {
    matches!(
        status,
        COPYENGINE_E_RECYCLE_BIN_NOT_FOUND
            | COPYENGINE_E_RECYCLE_FORCE_NUKE
            | COPYENGINE_E_RECYCLE_PATH_TOO_LONG
            | COPYENGINE_E_RECYCLE_SIZE_TOO_BIG
            | COPYENGINE_E_RECYCLE_UNKNOWN_ERROR
    )
}

#[allow(non_snake_case)]
impl IFileOperationProgressSink_Impl for RecycleSink_Impl {
    fn PreDeleteItem(&self, flags: u32, _: Ref<IShellItem>) -> WinResult<()> {
        if flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0 {
            self.cannot_recycle.store(true, Ordering::Relaxed);
            return Err(E_ABORT.into());
        }
        Ok(())
    }

    fn PostDeleteItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        status: HRESULT,
        _: Ref<IShellItem>,
    ) -> WinResult<()> {
        if is_recycle_error(status) {
            self.cannot_recycle.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    fn StartOperations(&self) -> WinResult<()> {
        Ok(())
    }

    fn FinishOperations(&self, _: HRESULT) -> WinResult<()> {
        Ok(())
    }

    fn PreRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> WinResult<()> {
        Ok(())
    }

    fn PostRenameItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> WinResult<()> {
        Ok(())
    }

    fn PreMoveItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> WinResult<()> {
        Ok(())
    }

    fn PostMoveItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> WinResult<()> {
        Ok(())
    }

    fn PreCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> WinResult<()> {
        Ok(())
    }

    fn PostCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> WinResult<()> {
        Ok(())
    }

    fn PreNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> WinResult<()> {
        Ok(())
    }

    fn PostNewItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: &PCWSTR,
        _: u32,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> WinResult<()> {
        Ok(())
    }

    fn UpdateProgress(&self, _: u32, _: u32) -> WinResult<()> {
        Ok(())
    }

    fn ResetTimer(&self) -> WinResult<()> {
        Ok(())
    }

    fn PauseTimer(&self) -> WinResult<()> {
        Ok(())
    }

    fn ResumeTimer(&self) -> WinResult<()> {
        Ok(())
    }
}

pub fn send_to_recycle_bin(target: &Path, forbidden: &Forbidden) -> Result<(), GuardError> {
    let target = super::validate_external_path(target.to_str().ok_or(GuardError::InvalidName)?)?;
    let ancestors = path::pin_ancestors(&target, forbidden)?;
    let before = path::inspect_path(&target, forbidden)?;
    path::protected_tree(&target, forbidden)?;
    let wide: Vec<_> = OsStr::new(&target).encode_wide().chain([0]).collect();
    // SAFETY: 当前线程独立初始化 COM，接口及缓冲区均在本次操作结束前存活。
    let result = unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(std::io::Error::from)?;
        let result = (|| -> Result<(), GuardError> {
            let operation: IFileOperation =
                CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)
                    .map_err(std::io::Error::from)?;
            let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None)
                .map_err(std::io::Error::from)?;
            if path::inspect_path(&target, forbidden)?.identity != before.identity {
                return Err(GuardError::Changed);
            }
            operation
                .SetOperationFlags(
                    FOF_ALLOWUNDO
                        | FOFX_RECYCLEONDELETE
                        | FOFX_EARLYFAILURE
                        | FOF_NOERRORUI
                        | FOF_SILENT,
                )
                .map_err(std::io::Error::from)?;
            operation
                .DeleteItem(&item, None)
                .map_err(std::io::Error::from)?;
            let sink = ComObject::new(RecycleSink {
                cannot_recycle: AtomicBool::new(false),
            });
            let sink_interface = sink.to_interface::<IFileOperationProgressSink>();
            let cookie = operation
                .Advise(&sink_interface)
                .map_err(std::io::Error::from)?;
            let performed = operation.PerformOperations();
            let _ = operation.Unadvise(cookie);
            if sink.cannot_recycle.load(Ordering::Relaxed)
                || performed
                    .as_ref()
                    .is_err_and(|error| is_recycle_error(error.code()))
            {
                return Err(GuardError::CannotRecycle);
            }
            performed.map_err(std::io::Error::from)?;
            if operation
                .GetAnyOperationsAborted()
                .map_err(std::io::Error::from)?
                .as_bool()
            {
                return Err(std::io::Error::other("Windows 未完成回收站操作").into());
            }
            Ok(())
        })();
        CoUninitialize();
        result
    };
    drop(ancestors);
    result
}
