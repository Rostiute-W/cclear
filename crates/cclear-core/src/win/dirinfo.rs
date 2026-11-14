//! 目录枚举：按句柄读取文件 ID、大小和属性。

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows::Win32::Foundation::{ERROR_NO_MORE_FILES, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_EXTD_DIR_INFO, FILE_ID_INFO,
    FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo, FileIdInfo,
    GetFileInformationByHandleEx, OPEN_EXISTING, SYNCHRONIZE,
};
use windows::core::{HRESULT, PCWSTR};

use super::to_verbatim;

/// 枚举缓冲区大小。64 KiB 足够一次返回几百个目录项，减少系统调用次数。
const BUFFER_BYTES: usize = 64 * 1024;

/// 一个目录项。大小同时给出逻辑大小（EndOfFile）和占盘大小（AllocationSize）。
#[derive(Clone, Debug)]
pub struct EntryInfo {
    pub name: OsString,
    pub file_id: u128,
    pub logical: u64,
    pub allocated: u64,
    pub attributes: u32,
    pub reparse_tag: u32,
    pub creation_time: u64,
    pub last_write_time: u64,
    pub is_dir: bool,
}

impl EntryInfo {
    /// 是否为 reparse point（符号链接、junction、云占位符等）。
    pub fn is_reparse(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    }
}

/// 打开目录句柄。不跟随 reparse point，这样 junction 和符号链接本身也能被枚举出来。
pub fn open_dir(path: &Path) -> io::Result<OwnedHandle> {
    let mut wide: Vec<u16> = to_verbatim(path).as_os_str().encode_wide().collect();
    wide.push(0);

    let desired_access = FILE_LIST_DIRECTORY.0 | SYNCHRONIZE.0;
    let handle = {
        // SAFETY: wide 是以 NUL 结尾的宽字符串且在调用期间存活；不传安全属性和模板句柄；
        // 共享模式放开读/写/删，避免因为别人正在写文件而读不到目录。
        unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                desired_access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }
    };

    let handle = handle.map_err(io::Error::from)?;
    // SAFETY: 句柄刚由 CreateFileW 成功返回，所有权转移给 OwnedHandle，由它负责关闭。
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
}

/// 逐项枚举目录，跳过 `.` 和 `..`。遇到 `ERROR_NO_MORE_FILES` 时正常结束。
pub fn for_each_entry(handle: &OwnedHandle, mut visit: impl FnMut(EntryInfo)) -> io::Result<()> {
    // 用 u64 数组保证缓冲区按 8 字节对齐，结构体里的 i64 字段才不会读到未对齐地址。
    let mut buffer = vec![0u64; BUFFER_BYTES / 8];
    let mut class = FileIdExtdDirectoryRestartInfo;

    loop {
        // SAFETY: 缓冲区指针与长度一致，句柄是本次调用打开的目录句柄。
        let listed = unsafe {
            GetFileInformationByHandleEx(
                HANDLE(handle.as_raw_handle()),
                class,
                buffer.as_mut_ptr() as *mut core::ffi::c_void,
                BUFFER_BYTES as u32,
            )
        };

        if let Err(error) = listed {
            // 枚举结束是正常情况，不是错误。
            if error.code() == HRESULT::from_win32(ERROR_NO_MORE_FILES.0) {
                return Ok(());
            }
            return Err(io::Error::from(error));
        }

        let base = buffer.as_ptr() as *const u8;
        let mut offset = 0usize;
        loop {
            // SAFETY: offset 始终指向本次写入的缓冲区内部，条目按 NextEntryOffset 串联。
            let info = unsafe { &*(base.add(offset) as *const FILE_ID_EXTD_DIR_INFO) };
            let name_length = info.FileNameLength as usize / 2;
            // SAFETY: FileName 是变长文件名的起始位置，长度取自系统返回的 FileNameLength。
            let name = unsafe { std::slice::from_raw_parts(info.FileName.as_ptr(), name_length) };
            let name = OsString::from_wide(name);

            if name != OsStr::new(".") && name != OsStr::new("..") {
                let entry = EntryInfo {
                    name,
                    file_id: u128::from_le_bytes(info.FileId.Identifier),
                    logical: info.EndOfFile as u64,
                    allocated: info.AllocationSize as u64,
                    attributes: info.FileAttributes,
                    reparse_tag: info.ReparsePointTag,
                    creation_time: info.CreationTime as u64,
                    last_write_time: info.LastWriteTime as u64,
                    is_dir: info.FileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0,
                };
                visit(entry);
            }

            let next = info.NextEntryOffset as usize;
            if next == 0 {
                break;
            }
            offset += next;
        }

        // 第一次必须用 Restart 才能从头开始枚举，之后用普通 Info 继续。
        class = FileIdExtdDirectoryInfo;
    }
}

/// 目录所在卷的序列号。硬链接去重按“卷序列号 + 文件 ID”判断同一个文件。
pub fn volume_serial(handle: &OwnedHandle) -> io::Result<u64> {
    let mut info = FILE_ID_INFO::default();
    // SAFETY: 传入的结构体指针与声明的大小一致，句柄是本次调用打开的目录句柄。
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(handle.as_raw_handle()),
            FileIdInfo,
            &mut info as *mut _ as *mut core::ffi::c_void,
            size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .map_err(io::Error::from)?;

    Ok(info.VolumeSerialNumber)
}
