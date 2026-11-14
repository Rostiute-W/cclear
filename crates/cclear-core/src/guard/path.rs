//! 句柄相对的打开与删除：全项目唯一执行删除的地方。
//!
//! 外部只拿得到句柄，不会用字符串路径去删除：从锚点开始逐级相对父句柄打开，
//! 路径在检查之后、使用之前被替换时，句柄仍然指向检查过的那一个对象。

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Component, Path, PathBuf, Prefix};

use windows::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows::Wdk::Storage::FileSystem::FILE_OPEN_REPARSE_POINT as FILE_OPEN_REPARSE_POINT_NT;
use windows::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_OPEN, FILE_SYNCHRONOUS_IO_NONALERT, NTCREATEFILE_CREATE_OPTIONS,
    NtCreateFile,
};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_DIR_NOT_EMPTY, ERROR_FILE_NOT_FOUND, ERROR_LOCK_VIOLATION,
    ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION, HANDLE, NTSTATUS, OBJ_CASE_INSENSITIVE,
    RtlNtStatusToDosError, UNICODE_STRING,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_ACCESS_RIGHTS, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO_EX,
    FILE_DISPOSITION_INFO_EX_FLAGS, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_FLAGS_AND_ATTRIBUTES, FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS, FILE_LIST_DIRECTORY,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_STANDARD_INFO, FILE_TRAVERSE, FileBasicInfo, FileDispositionInfoEx, FileIdInfo,
    FileStandardInfo, GetFileInformationByHandleEx, OPEN_EXISTING, SYNCHRONIZE,
    SetFileInformationByHandle,
};
use windows::Win32::System::IO::IO_STATUS_BLOCK;
use windows::core::PCWSTR;

use super::GuardError;
use super::forbidden::Forbidden;
use crate::model::FileIdentity;
use crate::win::dirinfo::{self, EntryInfo};

/// 共享模式全部放开：占用与否靠错误码判断，不靠这里的独占。
const SHARE_ALL: FILE_SHARE_MODE =
    FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0);

/// 一个已打开的目录。后续的打开都相对它的句柄进行。
pub struct DirHandle {
    handle: OwnedHandle,
    identity: FileIdentity,
    path: PathBuf,
}

impl DirHandle {
    /// 目录自身的文件身份。上层做“同一个目录”的判断时用它，不用路径字符串。
    pub fn identity(&self) -> FileIdentity {
        self.identity
    }

    /// 规范路径，只用于错误信息和展示，不参与任何文件操作。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// 打开意图。`Delete` 会额外申请删除权限，打开后就能在同一个句柄上删除。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenIntent {
    Inspect,
    Delete,
}

/// 一个已打开的目录项。句柄和打开时的元数据一起返回，删除时在同一个句柄上复核。
pub struct Entry {
    pub identity: FileIdentity,
    pub is_dir: bool,
    pub size: u64,
    pub allocated: u64,
    pub attributes: u32,
    pub ctime: u64,
    pub mtime: u64,
    handle: OwnedHandle,
    path: PathBuf,
}

impl Entry {
    /// 记下这份元数据，删除前用它确认还是同一个文件。
    pub fn expect(&self) -> Expect {
        Expect {
            identity: self.identity,
            size: self.size,
            mtime: self.mtime,
        }
    }
}

/// 删除前要求文件满足的条件，来自扫描时记录的元数据。
pub struct Expect {
    pub identity: FileIdentity,
    pub size: u64,
    pub mtime: u64,
}

/// 打开锚点目录：从卷根开始逐个组件相对打开，任何一级是 reparse point 或禁区都失败。
pub fn open_anchor(path: &Path, forbidden: &Forbidden) -> Result<DirHandle, GuardError> {
    let (root, parts) = split_anchor(path)?;

    let handle = open_volume_root(&root)?;
    let root_handle = DirHandle {
        identity: read_info(&handle)?.identity,
        handle,
        path: PathBuf::from(&root),
    };

    let (owned, dir_path) = walk_dirs(&root_handle, &parts, forbidden)?;
    let Some(handle) = owned else {
        return Ok(root_handle);
    };

    Ok(DirHandle {
        identity: read_info(&handle)?.identity,
        handle,
        path: dir_path,
    })
}

/// 相对锚点打开 `rel` 的最后一个组件。中间每一级都必须是真实目录。
pub fn open_rel(
    anchor: &DirHandle,
    rel: &[OsString],
    intent: OpenIntent,
    forbidden: &Forbidden,
) -> Result<Entry, GuardError> {
    let Some((last, parents)) = rel.split_last() else {
        return Err(GuardError::InvalidName);
    };

    let (owned, parent_path) = walk_dirs(anchor, parents, forbidden)?;
    let parent = owned.as_ref().unwrap_or(&anchor.handle);

    let role = match intent {
        OpenIntent::Inspect => Role::Inspect,
        OpenIntent::Delete => Role::Delete,
    };
    let target = parent_path.join(last);

    let entry = open_component(parent, last, role, &target)?;
    check_component(&entry, last, forbidden)?;
    Ok(entry)
}

/// 枚举锚点下 `rel` 指向的目录。目录句柄同样通过相对打开获得，不走字符串路径。
pub fn list_dir(
    anchor: &DirHandle,
    rel: &[OsString],
    forbidden: &Forbidden,
) -> Result<Vec<EntryInfo>, GuardError> {
    let (owned, _) = walk_dirs(anchor, rel, forbidden)?;
    let handle = owned.as_ref().unwrap_or(&anchor.handle);

    let mut entries = Vec::new();
    dirinfo::for_each_entry(handle, |entry| entries.push(entry)).map_err(map_os_error)?;
    Ok(entries)
}

/// 删除一个已打开的条目。先用同一个句柄复核身份、大小和修改时间，再按句柄删除。
pub fn delete(entry: Entry, expect: &Expect) -> Result<(), GuardError> {
    let info = read_info(&entry.handle)?;

    // 句柄会一直指向打开时的那个对象，所以同名替换不会改变 identity；
    // 名字被摘掉（同名替换、别处已经删掉）时只能从 delete pending 上看得出来。
    if info.delete_pending || info.identity != expect.identity {
        return Err(GuardError::Changed);
    }

    // 目录的修改时间会随子项删除而变化，大小也没有意义，所以目录只按身份复核：
    // 删除目录本身的安全性来自逐级相对打开（无链接、无禁区）和非空检查。
    if !info.is_dir && (info.size != expect.size || info.last_write_time != expect.mtime) {
        return Err(GuardError::Changed);
    }

    // 只读文件一律不动，不清属性，也不用 IGNORE_READONLY 标志绕过。
    if info.attributes & FILE_ATTRIBUTE_READONLY.0 != 0 {
        return Err(GuardError::ReadOnly);
    }

    // 最后一级本身是 reparse point 时按链接删除会动到目标，一律拒绝。
    if info.attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(GuardError::ReparseInPath(entry.path.clone()));
    }

    let disposition = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_INFO_EX_FLAGS(
            FILE_DISPOSITION_FLAG_DELETE.0 | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0,
        ),
    };

    // SAFETY: 结构体指针与声明的大小一致，句柄由 Entry 持有（Delete 意图带 DELETE 权限）
    // 且在调用期间有效。POSIX 语义让删除立刻生效，不会留下 delete pending 状态。
    let result = unsafe {
        SetFileInformationByHandle(
            HANDLE(entry.handle.as_raw_handle()),
            FileDispositionInfoEx,
            &disposition as *const FILE_DISPOSITION_INFO_EX as *const core::ffi::c_void,
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };

    result.map_err(|error| map_os_error(io::Error::from(error)))
}

/// 组件在相对打开时的角色。
#[derive(Clone, Copy)]
enum Role {
    /// 中间组件：必须是目录，只要能列目录和穿过。
    Directory,
    /// 最后一级只读打开：文件或目录都可以。
    Inspect,
    /// 最后一级为了删除而打开：额外申请 DELETE。
    Delete,
}

impl Role {
    fn access(self) -> FILE_ACCESS_RIGHTS {
        match self {
            Role::Directory => {
                FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE
            }
            Role::Inspect => FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            Role::Delete => DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        }
    }

    fn options(self) -> NTCREATEFILE_CREATE_OPTIONS {
        // 这一组标志没有位或实现，只能自己拼。
        let base = NTCREATEFILE_CREATE_OPTIONS(
            FILE_SYNCHRONOUS_IO_NONALERT.0 | FILE_OPEN_REPARSE_POINT_NT.0,
        );

        match self {
            Role::Directory => NTCREATEFILE_CREATE_OPTIONS(base.0 | FILE_DIRECTORY_FILE.0),
            Role::Inspect | Role::Delete => base,
        }
    }
}

/// 从锚点逐级相对打开 `rel` 的每一级目录，返回最后一级的句柄和路径。
///
/// 返回 `None` 表示 `rel` 为空，调用方继续用锚点自己的句柄。
fn walk_dirs(
    anchor: &DirHandle,
    rel: &[OsString],
    forbidden: &Forbidden,
) -> Result<(Option<OwnedHandle>, PathBuf), GuardError> {
    let mut current: Option<OwnedHandle> = None;
    let mut path = anchor.path.clone();

    for name in rel {
        let parent = match &current {
            Some(handle) => handle,
            None => &anchor.handle,
        };
        path.push(name);

        let entry = open_component(parent, name, Role::Directory, &path)?;
        check_component(&entry, name, forbidden)?;
        current = Some(entry.handle);
    }

    Ok((current, path))
}

/// 相对父句柄打开一个组件，并记下它的元数据。
fn open_component(
    parent: &OwnedHandle,
    name: &OsStr,
    role: Role,
    path: &Path,
) -> Result<Entry, GuardError> {
    validate_component(name)?;

    let handle = nt_open(parent, name, role.access(), role.options())?;
    let info = read_info(&handle)?;

    Ok(Entry {
        identity: info.identity,
        is_dir: info.is_dir,
        size: info.size,
        allocated: info.allocated,
        attributes: info.attributes,
        ctime: info.creation_time,
        mtime: info.last_write_time,
        handle,
        path: path.to_path_buf(),
    })
}

/// 组件的 reparse point 和禁区检查。中间组件和最后一级都要过这一关。
fn check_component(entry: &Entry, name: &OsStr, forbidden: &Forbidden) -> Result<(), GuardError> {
    if entry.attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(GuardError::ReparseInPath(entry.path.clone()));
    }

    if forbidden.matches(name, &entry.identity) {
        return Err(GuardError::Forbidden(entry.path.clone()));
    }

    Ok(())
}

/// 用 `NtCreateFile` 相对父句柄打开单个组件，名字始终是单个组件。
fn nt_open(
    parent: &OwnedHandle,
    name: &OsStr,
    access: FILE_ACCESS_RIGHTS,
    options: NTCREATEFILE_CREATE_OPTIONS,
) -> Result<OwnedHandle, GuardError> {
    let mut buffer: Vec<u16> = name.encode_wide().collect();
    let bytes = u16::try_from(buffer.len() * 2).map_err(|_| GuardError::InvalidName)?;

    // UNICODE_STRING 不要求 NUL 结尾，长度按字节算，缓冲区必须活到调用结束。
    let object_name = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: windows::core::PWSTR(buffer.as_mut_ptr()),
    };

    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: HANDLE(parent.as_raw_handle()),
        ObjectName: &object_name,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };

    let mut io_status = IO_STATUS_BLOCK::default();
    let mut handle = HANDLE::default();

    // SAFETY: 对象名指向本函数内一直存活的缓冲区；根目录是有效的父句柄；
    // IO_STATUS_BLOCK 与句柄都在栈上且大小正确；不传扩展属性和分配大小。
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &attributes,
            &mut io_status,
            None,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            SHARE_ALL,
            FILE_OPEN,
            options,
            None,
            0,
        )
    };

    if status.0 < 0 {
        return Err(nt_error(status));
    }

    // SAFETY: 句柄由 NtCreateFile 成功返回，所有权交给 OwnedHandle，由它负责关闭。
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
}

/// 打开卷根 `\\?\X:\`。整个护栏的起点，之后全部相对它逐级打开。
fn open_volume_root(root: &str) -> Result<OwnedHandle, GuardError> {
    let mut wide: Vec<u16> = root.encode_utf16().collect();
    wide.push(0);

    let access = FILE_LIST_DIRECTORY.0 | FILE_TRAVERSE.0 | FILE_READ_ATTRIBUTES.0 | SYNCHRONIZE.0;

    // SAFETY: wide 以 NUL 结尾且在调用期间存活；不传安全属性和模板句柄；
    // 共享模式放开读/写/删，避免因为别人正在写而打不开目录。
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            access,
            SHARE_ALL,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    };

    let handle = handle.map_err(|error| map_os_error(io::Error::from(error)))?;

    // SAFETY: 句柄刚由 CreateFileW 成功返回，所有权转移给 OwnedHandle。
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
}

/// 一个句柄上读到的元数据。
struct FileInfo {
    identity: FileIdentity,
    is_dir: bool,
    size: u64,
    allocated: u64,
    attributes: u32,
    creation_time: u64,
    last_write_time: u64,
    /// 对象已经失去名字，只等最后一个句柄关闭。
    delete_pending: bool,
}

/// 在读打开的那个句柄上取元数据。删除前复核也走这里。
fn read_info(handle: &OwnedHandle) -> Result<FileInfo, GuardError> {
    let id: FILE_ID_INFO = query(handle, FileIdInfo)?;
    let basic: FILE_BASIC_INFO = query(handle, FileBasicInfo)?;
    let standard: FILE_STANDARD_INFO = query(handle, FileStandardInfo)?;

    Ok(FileInfo {
        identity: FileIdentity {
            volume_serial: id.VolumeSerialNumber,
            file_id: u128::from_le_bytes(id.FileId.Identifier),
        },
        is_dir: basic.FileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0,
        size: standard.EndOfFile as u64,
        allocated: standard.AllocationSize as u64,
        attributes: basic.FileAttributes,
        creation_time: basic.CreationTime as u64,
        last_write_time: basic.LastWriteTime as u64,
        delete_pending: standard.DeletePending,
    })
}

/// 取一个句柄的文件身份，供禁区集合记录。
pub(crate) fn identity_of(handle: &OwnedHandle) -> io::Result<FileIdentity> {
    let mut id = FILE_ID_INFO::default();

    // SAFETY: 输出缓冲区指针与声明的大小一致，句柄由调用方持有且在调用期间有效。
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(handle.as_raw_handle()),
            FileIdInfo,
            &mut id as *mut FILE_ID_INFO as *mut core::ffi::c_void,
            size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .map_err(io::Error::from)?;

    Ok(FileIdentity {
        volume_serial: id.VolumeSerialNumber,
        file_id: u128::from_le_bytes(id.FileId.Identifier),
    })
}

/// 通用的 `GetFileInformationByHandleEx` 查询。
fn query<T: Default>(
    handle: &OwnedHandle,
    class: FILE_INFO_BY_HANDLE_CLASS,
) -> Result<T, GuardError> {
    let mut value = T::default();

    // SAFETY: 输出缓冲区大小与 T 一致，句柄由调用方持有且在调用期间有效。
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(handle.as_raw_handle()),
            class,
            &mut value as *mut T as *mut core::ffi::c_void,
            size_of::<T>() as u32,
        )
    }
    .map_err(|error| map_os_error(io::Error::from(error)))?;

    Ok(value)
}

/// 组件名必须是一个普通的单级名字。
fn validate_component(name: &OsStr) -> Result<(), GuardError> {
    if name.is_empty() {
        return Err(GuardError::InvalidName);
    }

    let text = name.to_string_lossy();

    // 分隔符和冒号会把组件名变成多级路径或备用数据流，`.` 和 `..` 会越出锚点。
    if text == "." || text == ".." || text.contains(['\\', '/', ':', '\0']) {
        return Err(GuardError::InvalidName);
    }

    Ok(())
}

/// 把锚点路径拆成卷根和组件列表。只接受盘符绝对路径。
fn split_anchor(path: &Path) -> Result<(String, Vec<OsString>), GuardError> {
    let mut components = path.components();

    let Some(Component::Prefix(prefix)) = components.next() else {
        return Err(GuardError::InvalidName);
    };
    let drive = match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
        // UNC、设备路径和 GLOBALROOT 都不该出现在锚点上。
        _ => return Err(GuardError::InvalidName),
    };

    if components.next() != Some(Component::RootDir) {
        return Err(GuardError::InvalidName);
    }

    let mut parts = Vec::new();
    for component in components {
        let Component::Normal(name) = component else {
            return Err(GuardError::InvalidName);
        };
        validate_component(name)?;
        parts.push(name.to_os_string());
    }

    Ok((format!(r"\\?\{}:\", drive as char), parts))
}

/// `NtCreateFile` 的 NTSTATUS 转成和 Win32 错误码一致的护栏错误。
fn nt_error(status: NTSTATUS) -> GuardError {
    // SAFETY: 只把 NTSTATUS 换算成当前线程的 DOS 错误码，不访问任何内存。
    let code = unsafe { RtlNtStatusToDosError(status) };
    map_os_error(io::Error::from_raw_os_error(code as i32))
}

/// 系统错误码到护栏错误。共享冲突和锁冲突都算“被占用”。
fn map_os_error(error: io::Error) -> GuardError {
    let Some(raw) = error.raw_os_error().map(|code| code as u32) else {
        return GuardError::Io(error);
    };

    // windows 库把 Win32 错误码装进 HRESULT（0x8007xxxx），这里取出真正的错误码。
    let raw = if raw & 0xFFFF_0000 == 0x8007_0000 {
        raw & 0xFFFF
    } else {
        raw
    };

    if raw == ERROR_SHARING_VIOLATION.0 || raw == ERROR_LOCK_VIOLATION.0 {
        return GuardError::Locked;
    }
    if raw == ERROR_DIR_NOT_EMPTY.0 {
        return GuardError::DirNotEmpty;
    }
    if raw == ERROR_FILE_NOT_FOUND.0 || raw == ERROR_PATH_NOT_FOUND.0 {
        return GuardError::Gone;
    }
    if raw == ERROR_ACCESS_DENIED.0 {
        return GuardError::Denied;
    }

    GuardError::Io(error)
}
