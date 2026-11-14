//! 句柄相对的打开与删除：全项目唯一执行删除的地方。
//!
//! 外部只拿得到句柄，不会用字符串路径去删除：从锚点开始逐级相对父句柄打开，
//! 路径在检查之后、使用之前被替换时，句柄仍然指向检查过的那一个对象。

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Component, Path, PathBuf, Prefix};

use windows::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows::Wdk::Storage::FileSystem::FILE_OPEN_REPARSE_POINT as FILE_OPEN_REPARSE_POINT_NT;
use windows::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_OPEN, FILE_SYNCHRONOUS_IO_NONALERT,
    NTCREATEFILE_CREATE_DISPOSITION, NTCREATEFILE_CREATE_OPTIONS, NtCreateFile,
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
    FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_RENAME_INFO, FILE_RENAME_INFO_0, FILE_SHARE_DELETE,
    FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_WRITE_DATA,
    FileBasicInfo, FileDispositionInfoEx, FileIdInfo, FileRenameInfoEx, FileStandardInfo,
    GetFileInformationByHandleEx, OPEN_EXISTING, SYNCHRONIZE, SetFileInformationByHandle,
};
use windows::Win32::System::IO::{DeviceIoControl, IO_STATUS_BLOCK};
use windows::core::PCWSTR;

use super::GuardError;
use super::forbidden::Forbidden;
use crate::model::FileIdentity;
use crate::win::dirinfo::{self, EntryInfo};

/// 共享模式全部放开：占用与否靠错误码判断，不靠这里的独占。
const SHARE_ALL: FILE_SHARE_MODE =
    FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0);

/// `FILE_RENAME_INFO` 的 Flags 取值。windows crate 没有导出这两个常量，照 Win32 头文件写。
/// 前者表示目标存在时直接替换，后者要求按 POSIX 语义替换（允许目标正被打开）。
const FILE_RENAME_FLAG_REPLACE_IF_EXISTS: u32 = 0x0000_0001;
const FILE_RENAME_FLAG_POSIX_SEMANTICS: u32 = 0x0000_0002;

/// `CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 41/42, METHOD_BUFFERED, ...)` 算出来的控制码。
/// windows crate 把它们放在 `Win32::System::Ioctl` 下，为了两个常量多开一个 feature 不划算。
const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00A8;

/// 目录挂载点（junction）的 reparse tag：`IO_REPARSE_TAG_MOUNT_POINT`。
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

/// `REPARSE_DATA_BUFFER` 的固定头部：tag、数据长度、保留字段。
const REPARSE_HEADER_BYTES: usize = 8;

/// MountPoint 数据区开头的 4 个长度字段：两个名字的偏移和长度。
const MOUNT_POINT_HEADER_BYTES: usize = 8;

/// 系统允许的 reparse 数据上限。
const REPARSE_BUFFER_LIMIT: usize = 16 * 1024;

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

    dispose_handle(&entry.handle)
}

/// 在句柄上设置立即删除。
///
/// POSIX 语义让删除立刻生效，不会留下 delete pending 状态；句柄指向哪个对象就删哪个对象，
/// 所以调用方必须自己保证句柄是“要删的那一个”（相对打开 + 复核身份）。
fn dispose_handle(handle: &OwnedHandle) -> Result<(), GuardError> {
    let disposition = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_INFO_EX_FLAGS(
            FILE_DISPOSITION_FLAG_DELETE.0 | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0,
        ),
    };

    // SAFETY: 结构体指针与声明的大小一致，句柄由调用方持有且在调用期间有效。
    let result = unsafe {
        SetFileInformationByHandle(
            HANDLE(handle.as_raw_handle()),
            FileDispositionInfoEx,
            &disposition as *const FILE_DISPOSITION_INFO_EX as *const core::ffi::c_void,
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };

    result.map_err(|error| map_os_error(io::Error::from(error)))
}

/// 在已打开的目录里把临时名字替换成最终名字。
///
/// 状态文件（例如隔离区的 manifest）靠它原子更新：先写同目录下的临时文件，再整体替换，
/// 中途崩溃时读到的要么是旧的完整内容，要么是新的完整内容。
pub fn replace_in_dir(
    dir: &DirHandle,
    tmp_name: &OsStr,
    final_name: &OsStr,
) -> Result<(), GuardError> {
    validate_component(tmp_name)?;
    validate_component(final_name)?;

    // 重命名要求源文件带 DELETE 权限；目标是目录里的一个名字，不需要再打开它。
    let entry = open_component(
        &dir.handle,
        tmp_name,
        Role::Delete,
        &dir.path.join(tmp_name),
    )?;

    rename_handle(
        &entry.handle,
        &dir.path.join(final_name),
        FILE_RENAME_FLAG_REPLACE_IF_EXISTS | FILE_RENAME_FLAG_POSIX_SEMANTICS,
    )
}

/// 在同一个目录里把 `from` 改名成 `to`。
///
/// 不带 `REPLACE_IF_EXISTS`：目标已经存在时由系统直接拒绝，绝不覆盖已有目录。
/// 迁移把源目录改名成备份名时用它，所以改名之前先按预览时记下的身份复核一次。
pub fn rename_child(
    parent: &DirHandle,
    from: &OsStr,
    to: &OsStr,
    expect: &Expect,
) -> Result<(), GuardError> {
    validate_component(from)?;
    validate_component(to)?;

    let entry = open_component(&parent.handle, from, Role::Delete, &parent.path.join(from))?;
    if entry.identity != expect.identity {
        return Err(GuardError::Changed);
    }

    rename_handle(
        &entry.handle,
        &parent.path.join(to),
        FILE_RENAME_FLAG_POSIX_SEMANTICS,
    )
}

/// 在已打开的目录里新建一个子目录。同名对象已存在时由系统直接失败，绝不覆盖。
///
/// 相对父句柄打开，不经过字符串路径，因此不会顺着路径上的 junction 落到别的位置。
pub fn create_dir_in(parent: &DirHandle, name: &OsStr) -> Result<(), GuardError> {
    validate_component(name)?;

    // 句柄只用于创建，返回后立即关闭。
    let _ = nt_create_dir(&parent.handle, name)?;

    Ok(())
}

/// 在已打开的目录里新建一个文件，返回可写句柄。
///
/// 与 [`create_dir_in`] 一样用 `FILE_CREATE`：同名文件已存在时直接失败，
/// 调用方拿到的句柄一定指向本次新建的那个文件，不会写进别人的文件里。
pub fn create_file_in(parent: &DirHandle, name: &OsStr) -> Result<File, GuardError> {
    validate_component(name)?;

    let access = FILE_WRITE_DATA | FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
    let options = NTCREATEFILE_CREATE_OPTIONS(FILE_SYNCHRONOUS_IO_NONALERT.0);

    let handle = nt_create(&parent.handle, name, access, FILE_CREATE, options)?;

    // SAFETY: 句柄由 NtCreateFile 成功返回，所有权转交给 File，由它负责关闭。
    Ok(File::from(handle))
}

/// 在句柄上执行改名。`FileRenameInfoEx` 不接受 RootDirectory，目标必须给完整路径。
fn rename_handle(handle: &OwnedHandle, target: &Path, flags: u32) -> Result<(), GuardError> {
    let target = crate::win::to_verbatim(target);
    let name: Vec<u16> = target.as_os_str().encode_wide().collect();
    let name_bytes = u32::try_from(name.len() * 2).map_err(|_| GuardError::InvalidName)?;

    // FILE_RENAME_INFO 的 FileName 是变长尾部：缓冲区长度必须是“结构体长度减去自带的 1 个字符
    // 再加实际名字”，多传的字节会被当成参数错误。
    let header = size_of::<FILE_RENAME_INFO>() - size_of::<u16>();
    let total = header + name.len() * 2;

    // 用 u64 数组分配，保证结构体要求的 8 字节对齐。
    let mut storage = vec![0u64; total.div_ceil(size_of::<u64>())];
    let info = storage.as_mut_ptr() as *mut FILE_RENAME_INFO;

    // SAFETY: storage 按结构体加名字长度分配且按 8 字节对齐，下面写入的内容都在缓冲区范围内。
    unsafe {
        (*info).Anonymous = FILE_RENAME_INFO_0 { Flags: flags };
        (*info).RootDirectory = HANDLE::default();
        (*info).FileNameLength = name_bytes;
        let destination = std::ptr::addr_of_mut!((*info).FileName) as *mut u16;
        std::ptr::copy_nonoverlapping(name.as_ptr(), destination, name.len());
    }

    // SAFETY: 缓冲区按 FILE_RENAME_INFO 的布局分配，长度随名字一起传入；
    // 句柄由调用方持有且在调用期间有效。
    let result = unsafe {
        SetFileInformationByHandle(
            HANDLE(handle.as_raw_handle()),
            FileRenameInfoEx,
            info as *const core::ffi::c_void,
            total as u32,
        )
    };

    result.map_err(|error| map_os_error(io::Error::from(error)))
}

/// 在 `parent` 里创建名为 `name` 的 junction，指向 `target`。
///
/// 先以 `FILE_CREATE` 相对创建一个空目录（同名对象已存在时系统直接失败，不会覆盖），
/// 再在这个目录上写入 MountPoint 的 reparse 数据。
pub fn create_junction(parent: &DirHandle, name: &OsStr, target: &Path) -> Result<(), GuardError> {
    validate_component(name)?;

    let buffer = mount_point_buffer(target)?;
    let dir = nt_create_dir(&parent.handle, name)?;

    let mut returned = 0u32;

    // SAFETY: 控制码与下面组好的 MountPoint 数据一致；输入缓冲区指针和长度取自同一个 Vec，
    // 目录句柄是刚创建出来的、在调用期间一直有效；不要输出缓冲区，也不做异步等待。
    let result = unsafe {
        DeviceIoControl(
            HANDLE(dir.as_raw_handle()),
            FSCTL_SET_REPARSE_POINT,
            Some(buffer.as_ptr() as *const core::ffi::c_void),
            buffer.len() as u32,
            None,
            0,
            Some(&mut returned),
            None,
        )
    };

    if let Err(error) = result {
        // 写不进 reparse 数据时必须撤掉刚建的空目录：留着它会让上层以为链接已经建好。
        let _ = dispose_handle(&dir);
        return Err(map_os_error(io::Error::from(error)));
    }

    Ok(())
}

/// 读 `parent` 下 `name` 这个 junction 指向的目标。不是 junction 时报 `LinkMismatch`。
pub fn read_junction_target(parent: &DirHandle, name: &OsStr) -> Result<PathBuf, GuardError> {
    validate_component(name)?;

    let entry = open_component(&parent.handle, name, Role::Link, &parent.path.join(name))?;
    reparse_target(&entry.handle, &entry.path)
}

/// 删除 `parent` 下的 junction，只删链接本身。
///
/// 打开时带 `FILE_OPEN_REPARSE_POINT`，句柄指向链接而不是目标；再核对目标与预期一致，
/// 目标对不上说明这个链接不是 CClear 建的那个，宁可不删。
pub fn remove_junction(
    parent: &DirHandle,
    name: &OsStr,
    expect_target: &Path,
) -> Result<(), GuardError> {
    validate_component(name)?;

    let entry = open_component(&parent.handle, name, Role::Link, &parent.path.join(name))?;

    let actual = reparse_target(&entry.handle, &entry.path)?;
    let same =
        crate::win::equals_ignore_case(&actual.to_string_lossy(), &expect_target.to_string_lossy());
    if !same {
        return Err(GuardError::LinkMismatch(entry.path.clone()));
    }

    dispose_handle(&entry.handle)
}

/// 组 MountPoint 的 `REPARSE_DATA_BUFFER`。
///
/// SubstituteName 是内核用的 NT 路径（`\??\X:\...`），PrintName 是资源管理器显示的普通路径；
/// 两个名字都放在变长的 PathBuffer 里，前面的偏移量相对 PathBuffer 起点。
fn mount_point_buffer(target: &Path) -> Result<Vec<u8>, GuardError> {
    let print = target.to_string_lossy().into_owned();
    let bytes = print.as_bytes();

    // 只接受盘符绝对路径：UNC 和设备路径写成 junction 会让链接指向不可预期的位置。
    if bytes.len() < 4 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        return Err(GuardError::InvalidTarget(print));
    }
    // 盘符根（`X:\`）不是可以挂载的目录。
    if print.ends_with('\\') {
        return Err(GuardError::InvalidTarget(print));
    }

    let substitute: Vec<u16> = format!(r"\??\{print}").encode_utf16().collect();
    let printable: Vec<u16> = print.encode_utf16().collect();

    let substitute_bytes = substitute.len() * 2;
    let print_bytes = printable.len() * 2;

    // 两个名字在 PathBuffer 里各带一个结尾 NUL，PrintNameOffset 要跳过第一个名字的 NUL。
    let path_bytes = substitute_bytes + 2 + print_bytes + 2;
    let total = REPARSE_HEADER_BYTES + MOUNT_POINT_HEADER_BYTES + path_bytes;
    if total > REPARSE_BUFFER_LIMIT {
        return Err(GuardError::InvalidTarget(print));
    }

    let mut buffer = vec![0u8; total];
    buffer[0..4].copy_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    // ReparseDataLength 不含固定头部，只算 MountPoint 数据区加两个名字。
    let data_length = (MOUNT_POINT_HEADER_BYTES + path_bytes) as u16;
    buffer[4..6].copy_from_slice(&data_length.to_le_bytes());

    let mut at = REPARSE_HEADER_BYTES;
    for value in [
        0u16,                          // SubstituteNameOffset
        substitute_bytes as u16,       // SubstituteNameLength
        (substitute_bytes + 2) as u16, // PrintNameOffset
        print_bytes as u16,            // PrintNameLength
    ] {
        buffer[at..at + 2].copy_from_slice(&value.to_le_bytes());
        at += 2;
    }

    // PathBuffer 里两个名字各以 NUL 结尾，所以第一个名字后面要先写一个 0u16 再写第二个；
    // PrintNameOffset 正是跳过这个 NUL，漏写会让 PrintName 整体错位一个字符。
    for unit in substitute.iter().chain([&0u16]).chain(printable.iter()) {
        buffer[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        at += 2;
    }

    // 末尾再补一个 NUL，PathBuffer 的两个名字都按 C 字符串写。
    buffer[at..at + 2].copy_from_slice(&0u16.to_le_bytes());

    Ok(buffer)
}

/// 读句柄上的 MountPoint 目标。tag 不是目录挂载点时按“链接不符预期”拒绝。
fn reparse_target(handle: &OwnedHandle, path: &Path) -> Result<PathBuf, GuardError> {
    let mut buffer = vec![0u8; REPARSE_BUFFER_LIMIT];
    let mut returned = 0u32;

    // SAFETY: 输出缓冲区按系统允许的上限分配，长度随指针一起传入；
    // 句柄由调用方持有且在调用期间有效。
    let result = unsafe {
        DeviceIoControl(
            HANDLE(handle.as_raw_handle()),
            FSCTL_GET_REPARSE_POINT,
            None,
            0,
            Some(buffer.as_mut_ptr() as *mut core::ffi::c_void),
            buffer.len() as u32,
            Some(&mut returned),
            None,
        )
    };
    result.map_err(|error| map_os_error(io::Error::from(error)))?;

    let returned = (returned as usize).min(buffer.len());
    parse_mount_point(&buffer[..returned], path)
}

/// 从 `REPARSE_DATA_BUFFER` 里取出挂载目标。
fn parse_mount_point(buffer: &[u8], path: &Path) -> Result<PathBuf, GuardError> {
    if buffer.len() < REPARSE_HEADER_BYTES + MOUNT_POINT_HEADER_BYTES {
        return Err(GuardError::LinkMismatch(path.to_path_buf()));
    }

    let tag = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
    if tag != IO_REPARSE_TAG_MOUNT_POINT {
        return Err(GuardError::LinkMismatch(path.to_path_buf()));
    }

    let word = |at: usize| u16::from_le_bytes([buffer[at], buffer[at + 1]]);

    // PrintName 是资源管理器显示的普通路径（`X:\CClearMig\<id>`），SubstituteName 是
    // 内核用的 NT 路径（`\??\X:\...`）。优先用 PrintName：它的偏移量能顺带验证两个名字
    // 之间的 NUL 有没有写对；少数工具只写 SubstituteName，这时退回用它。
    let print_length = usize::from(word(REPARSE_HEADER_BYTES + 6));
    let (offset, length) = if print_length > 0 {
        (usize::from(word(REPARSE_HEADER_BYTES + 4)), print_length)
    } else {
        (
            usize::from(word(REPARSE_HEADER_BYTES)),
            usize::from(word(REPARSE_HEADER_BYTES + 2)),
        )
    };

    let start = REPARSE_HEADER_BYTES + MOUNT_POINT_HEADER_BYTES + offset;
    let end = start + length;
    if length == 0 || end > buffer.len() {
        return Err(GuardError::LinkMismatch(path.to_path_buf()));
    }

    let units: Vec<u16> = buffer[start..end]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let text = String::from_utf16_lossy(&units);

    // `\??\` 前缀只在内核里有意义，对外统一返回普通路径。
    let text = match text.strip_prefix(r"\??\") {
        Some(rest) => rest.to_string(),
        None => text,
    };

    Ok(PathBuf::from(text))
}

/// 后序删除 `parent` 下的一个条目（文件或整棵目录树）。
///
/// 只能用在隔离区这类完全由 CClear 自己写入的目录上：不按用户规则排除禁区，
/// 但依然逐级相对打开，并且遇到 reparse point 时直接失败。
pub fn delete_tree(
    parent: &DirHandle,
    name: &OsStr,
    forbidden: &Forbidden,
) -> Result<(), GuardError> {
    let mut rel = vec![name.to_os_string()];

    let entry = open_rel(parent, &rel, OpenIntent::Inspect, forbidden)?;
    if entry.is_dir {
        delete_children(parent, &mut rel, forbidden)?;
    }

    // 目录这时已经空了；重新按删除意图打开，在同一次打开上复核后再删。
    let entry = open_rel(parent, &rel, OpenIntent::Delete, forbidden)?;
    let expect = entry.expect();
    delete(entry, &expect)
}

/// 递归删除 `rel` 指向的目录里的全部内容，目录本身保留给调用方删。
fn delete_children(
    parent: &DirHandle,
    rel: &mut Vec<OsString>,
    forbidden: &Forbidden,
) -> Result<(), GuardError> {
    let entries = list_dir(parent, rel, forbidden)?;

    for entry in entries {
        // 链接不能跟着删：它指向的内容在别处，删掉就是误删。
        if entry.is_reparse() {
            let mut path = parent.path().to_path_buf();
            for part in rel.iter() {
                path.push(part);
            }
            path.push(&entry.name);
            return Err(GuardError::ReparseInPath(path));
        }

        rel.push(entry.name);
        if entry.is_dir {
            delete_children(parent, rel, forbidden)?;
        }

        let opened = open_rel(parent, rel, OpenIntent::Delete, forbidden)?;
        let expect = opened.expect();
        delete(opened, &expect)?;

        rel.pop();
    }

    Ok(())
}

/// 组件在相对打开时的角色。
#[derive(Clone, Copy)]
enum Role {
    /// 中间组件：必须是目录，只要能列目录和读属性。
    Directory,
    /// 最后一级只读打开：文件或目录都可以。
    Inspect,
    /// 最后一级为了删除而打开：额外申请 DELETE。
    Delete,
    /// 最后一级当链接本身打开：要读 reparse 数据，删除时还要 DELETE。
    Link,
}

impl Role {
    fn access(self) -> FILE_ACCESS_RIGHTS {
        match self {
            // 中间组件只需要列目录和读属性：相对打开用的是父句柄 + 单个名字，
            // 不要求父句柄带遍历权限，而安装程序留下的目录常常只给列目录和读属性。
            Role::Directory => FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            Role::Inspect => FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            Role::Delete => DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            // 读 reparse 数据要读权限，删除链接本身要 DELETE。
            Role::Link => DELETE | FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        }
    }

    fn options(self) -> NTCREATEFILE_CREATE_OPTIONS {
        // 这一组标志没有位或实现，只能自己拼。
        let base = NTCREATEFILE_CREATE_OPTIONS(
            FILE_SYNCHRONOUS_IO_NONALERT.0 | FILE_OPEN_REPARSE_POINT_NT.0,
        );

        match self {
            Role::Directory => NTCREATEFILE_CREATE_OPTIONS(base.0 | FILE_DIRECTORY_FILE.0),
            Role::Inspect | Role::Delete | Role::Link => base,
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
    nt_create(parent, name, access, FILE_OPEN, options)
}

/// 相对父句柄以 `FILE_CREATE` 创建空目录。
///
/// `FILE_CREATE` 在同名对象已存在时直接失败，这正是“绝不覆盖已有目标”的要求。
fn nt_create_dir(parent: &OwnedHandle, name: &OsStr) -> Result<OwnedHandle, GuardError> {
    // 写 reparse 数据需要目录的写权限，删除刚建出来的目录需要 DELETE。
    let access = FILE_WRITE_DATA | FILE_READ_DATA | FILE_READ_ATTRIBUTES | DELETE | SYNCHRONIZE;
    let options =
        NTCREATEFILE_CREATE_OPTIONS(FILE_SYNCHRONOUS_IO_NONALERT.0 | FILE_DIRECTORY_FILE.0);

    nt_create(parent, name, access, FILE_CREATE, options)
}

/// `NtCreateFile` 的公共部分：对象名、根目录和输出句柄都在这里组装。
fn nt_create(
    parent: &OwnedHandle,
    name: &OsStr,
    access: FILE_ACCESS_RIGHTS,
    disposition: NTCREATEFILE_CREATE_DISPOSITION,
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
            disposition,
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

    // 卷根只需要列目录和读属性：之后的每一级都相对它打开，不用遍历权限。
    let access = FILE_LIST_DIRECTORY.0 | FILE_READ_ATTRIBUTES.0 | SYNCHRONIZE.0;

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
pub(crate) fn validate_component(name: &OsStr) -> Result<(), GuardError> {
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
