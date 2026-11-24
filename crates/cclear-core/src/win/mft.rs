use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use thiserror::Error;
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};
use windows::core::PCWSTR;

use crate::model::SizeInfo;
use crate::scan::{DirNode, FileEntry, ScanProgress, ScanResult};

const CHUNK_BYTES: usize = 2 * 1024 * 1024;
const ID_MASK: u64 = 0x0000_ffff_ffff_ffff;

#[derive(Debug, Error)]
pub enum MftError {
    #[error("MFT 扫描需要管理员权限")]
    NotElevated,
    #[error("MFT 扫描需要本地固定 NTFS 卷根目录")]
    NotNtfs,
    #[error("MFT 数据不完整或使用了无法安全解析的布局")]
    Invalid,
    #[error("读取 MFT 失败：{0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, PartialEq, Eq)]
pub struct DataRun {
    pub lcn: u64,
    pub clusters: u64,
}

#[derive(Debug, Default)]
pub struct Record {
    pub sequence: u16,
    pub directory: bool,
    pub names: Vec<(u64, String)>,
    pub logical: u64,
    pub allocated: u64,
    pub last_write: u64,
    pub runs: Vec<DataRun>,
}

pub fn parse_runs(bytes: &[u8]) -> Result<Vec<DataRun>, MftError> {
    let mut runs = Vec::new();
    let mut position = 0;
    let mut lcn = 0i64;
    while let Some(&header) = bytes.get(position) {
        position += 1;
        if header == 0 {
            return if runs.is_empty() {
                Err(MftError::Invalid)
            } else {
                Ok(runs)
            };
        }
        let length_width = usize::from(header & 15);
        let delta_width = usize::from(header >> 4);
        if !(1..=8).contains(&length_width) || !(1..=8).contains(&delta_width) {
            return Err(MftError::Invalid);
        }
        let length = field(bytes, position, length_width)?;
        position += length_width;
        let delta = field(bytes, position, delta_width)?;
        position += delta_width;
        let shift = (8 - delta_width) * 8;
        let signed = ((delta << shift) as i64) >> shift;
        lcn = lcn.checked_add(signed).ok_or(MftError::Invalid)?;
        if lcn < 0 || length == 0 {
            return Err(MftError::Invalid);
        }
        runs.push(DataRun {
            lcn: lcn as u64,
            clusters: length,
        });
    }
    Err(MftError::Invalid)
}

fn field(bytes: &[u8], offset: usize, width: usize) -> Result<u64, MftError> {
    let slice = bytes
        .get(offset..offset.checked_add(width).ok_or(MftError::Invalid)?)
        .ok_or(MftError::Invalid)?;
    let mut buffer = [0; 8];
    buffer[..width].copy_from_slice(slice);
    Ok(u64::from_le_bytes(buffer))
}

pub fn parse_record(
    bytes: &mut [u8],
    sector: usize,
    read_runs: bool,
) -> Result<Option<Record>, MftError> {
    if bytes.iter().all(|byte| *byte == 0) || bytes.starts_with(b"BAAD") {
        if bytes.starts_with(b"BAAD") {
            return Err(MftError::Invalid);
        }
        return Ok(None);
    }
    if !bytes.starts_with(b"FILE") || sector < 2 || !bytes.len().is_multiple_of(sector) {
        return Err(MftError::Invalid);
    }
    let usa = field(bytes, 4, 2)? as usize;
    let count = field(bytes, 6, 2)? as usize;
    let first_attr = field(bytes, 20, 2)? as usize;
    if count != bytes.len() / sector + 1 || usa < 42 || usa + count * 2 > first_attr {
        return Err(MftError::Invalid);
    }
    let stamp = field(bytes, usa, 2)?;
    for index in 1..count {
        let trailer = index * sector - 2;
        if field(bytes, trailer, 2)? != stamp {
            return Err(MftError::Invalid);
        }
        let replacement = (field(bytes, usa + index * 2, 2)? as u16).to_le_bytes();
        bytes[trailer..trailer + 2].copy_from_slice(&replacement);
    }
    let flags = field(bytes, 22, 2)?;
    if flags & 1 == 0 || field(bytes, 32, 8)? != 0 {
        return Ok(None);
    }
    let mut record = Record {
        sequence: field(bytes, 16, 2)? as u16,
        directory: flags & 2 != 0,
        ..Record::default()
    };
    let end = field(bytes, 24, 4)? as usize;
    let used = bytes.get(..end).ok_or(MftError::Invalid)?;
    let mut offset = first_attr;
    let mut short_names = Vec::new();
    let mut reparse = false;
    loop {
        let kind = field(used, offset, 4)?;
        if kind == u64::from(u32::MAX) {
            break;
        }
        let length = field(used, offset + 4, 4)? as usize;
        if length < 24 || !length.is_multiple_of(8) {
            return Err(MftError::Invalid);
        }
        let attr = used.get(offset..offset + length).ok_or(MftError::Invalid)?;
        offset += length;
        let nonresident = attr[8] != 0;
        let named = attr[9] != 0;
        if kind == 0xc0 {
            reparse = true;
        }
        if read_runs && kind == 0x20 {
            return Err(MftError::Invalid);
        }
        if kind == 0x80 && nonresident {
            if field(attr, 16, 8)? != 0 {
                continue;
            }
            let flags = field(attr, 12, 2)?;
            let logical = field(attr, 48, 8)?;
            let allocated = if flags & 0x8001 != 0 {
                field(attr, 64, 8)?
            } else {
                field(attr, 40, 8)?
            };
            record.logical = record.logical.saturating_add(logical);
            record.allocated = record.allocated.saturating_add(allocated);
            if read_runs && !named {
                let start = field(attr, 32, 2)? as usize;
                if start < 64 {
                    return Err(MftError::Invalid);
                }
                record.runs = parse_runs(attr.get(start..).ok_or(MftError::Invalid)?)?;
            }
        } else if !nonresident && matches!(kind, 0x10 | 0x30 | 0x80) {
            let length = field(attr, 16, 4)? as usize;
            let start = field(attr, 20, 2)? as usize;
            if start < 24 {
                return Err(MftError::Invalid);
            }
            let value = attr.get(start..start + length).ok_or(MftError::Invalid)?;
            match kind {
                0x10 => record.last_write = field(value, 8, 8)?,
                0x80 => record.logical = record.logical.saturating_add(length as u64),
                0x30 => {
                    let count = field(value, 64, 1)? as usize;
                    let namespace = field(value, 65, 1)?;
                    let utf16: Vec<_> = value
                        .get(66..66 + 2 * count)
                        .ok_or(MftError::Invalid)?
                        .chunks_exact(2)
                        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                        .collect();
                    if let Ok(name) = String::from_utf16(&utf16)
                        && crate::guard::validate_component(name.as_ref()).is_ok()
                    {
                        let link = (field(value, 0, 8)?, name);
                        if namespace == 2 {
                            short_names.push(link);
                        } else {
                            record.names.push(link);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if record.names.is_empty() {
        record.names = short_names;
    }
    Ok((!reparse).then_some(record))
}

pub fn scan_volume(
    root: &Path,
    progress: &ScanProgress,
    cancel: &AtomicBool,
) -> Result<ScanResult, MftError> {
    if !super::is_elevated() {
        return Err(MftError::NotElevated);
    }
    let name = root.to_str().ok_or(MftError::NotNtfs)?;
    let name = crate::guard::validate_external_path(name).map_err(|_| MftError::NotNtfs)?;
    if name.as_os_str().len() != 3
        || !super::volume::list_fixed_volumes().iter().any(|volume| {
            volume.fs.eq_ignore_ascii_case("NTFS")
                && super::equals_ignore_case(
                    &volume.root.to_string_lossy(),
                    &name.to_string_lossy(),
                )
        })
    {
        return Err(MftError::NotNtfs);
    }
    let started = Instant::now();
    let drive = name.to_string_lossy();
    let device: Vec<_> = format!(r"\\.\{}", &drive[..2])
        .encode_utf16()
        .chain([0])
        .collect();
    // SAFETY: 打开本地卷只读句柄，卷名取自已验证的盘符，缓冲区以 NUL 结尾。
    let handle = unsafe {
        CreateFileW(
            PCWSTR(device.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(std::io::Error::from)?;
    // SAFETY: 句柄由本函数刚打开，所有权交给 File。
    let mut volume = unsafe { File::from_raw_handle(handle.0) };
    let mut data = NTFS_VOLUME_DATA_BUFFER::default();
    let mut returned = 0;
    // SAFETY: 卷句柄有效，输出结构可写，长度与 Win32 声明一致。
    unsafe {
        DeviceIoControl(
            handle,
            FSCTL_GET_NTFS_VOLUME_DATA,
            None,
            0,
            Some((&mut data as *mut NTFS_VOLUME_DATA_BUFFER).cast()),
            size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
            Some(&mut returned),
            None,
        )
    }
    .map_err(std::io::Error::from)?;
    if returned < size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32 {
        return Err(MftError::Invalid);
    }
    let sector = data.BytesPerSector as usize;
    let cluster = u64::from(data.BytesPerCluster);
    let record_size = data.BytesPerFileRecordSegment as usize;
    let max_clusters = u64::try_from(data.TotalClusters).map_err(|_| MftError::Invalid)?;
    let valid_bytes = u64::try_from(data.MftValidDataLength).map_err(|_| MftError::Invalid)?;
    let start = u64::try_from(data.MftStartLcn).map_err(|_| MftError::Invalid)?;
    if !(256..=65_536).contains(&sector)
        || !(sector..=65_536).contains(&record_size)
        || cluster == 0
        || !record_size.is_multiple_of(sector)
        || valid_bytes == 0
        || !valid_bytes.is_multiple_of(record_size as u64)
    {
        return Err(MftError::Invalid);
    }
    volume.seek(SeekFrom::Start(
        start.checked_mul(cluster).ok_or(MftError::Invalid)?,
    ))?;
    let mut first = vec![0; record_size];
    volume.read_exact(&mut first)?;
    let mft = parse_record(&mut first, sector, true)?.ok_or(MftError::Invalid)?;
    if mft.runs.first().is_none_or(|run| run.lcn != start) {
        return Err(MftError::Invalid);
    }
    let mut records = HashMap::new();
    let mut buffer = vec![0; CHUNK_BYTES + record_size];
    let mut carry = 0;
    let mut consumed = 0;
    let mut record_id = 0u64;
    for run in mft.runs {
        if consumed == valid_bytes || cancel.load(Ordering::Relaxed) {
            break;
        }
        if run
            .lcn
            .checked_add(run.clusters)
            .is_none_or(|end| end > max_clusters)
        {
            return Err(MftError::Invalid);
        }
        volume.seek(SeekFrom::Start(
            run.lcn.checked_mul(cluster).ok_or(MftError::Invalid)?,
        ))?;
        let mut remaining = run
            .clusters
            .checked_mul(cluster)
            .ok_or(MftError::Invalid)?
            .min(valid_bytes - consumed);
        while remaining != 0 && !cancel.load(Ordering::Relaxed) {
            let count = remaining.min(CHUNK_BYTES as u64) as usize;
            volume.read_exact(&mut buffer[carry..carry + count])?;
            let available = carry + count;
            let complete = available / record_size * record_size;
            for bytes in buffer[..complete].chunks_exact_mut(record_size) {
                if let Some(record) = parse_record(bytes, sector, false)? {
                    if record.directory {
                        progress.dirs.fetch_add(1, Ordering::Relaxed);
                    } else {
                        progress.files.fetch_add(1, Ordering::Relaxed);
                        progress
                            .allocated
                            .fetch_add(record.allocated, Ordering::Relaxed);
                    }
                    records.insert(record_id, record);
                }
                record_id += 1;
            }
            carry = available - complete;
            buffer.copy_within(complete..available, 0);
            remaining -= count as u64;
            consumed += count as u64;
        }
    }
    let cancelled = cancel.load(Ordering::Relaxed);
    if !cancelled && (consumed != valid_bytes || carry != 0) {
        return Err(MftError::Invalid);
    }
    build_tree(
        root,
        records,
        started.elapsed().as_millis() as u64,
        cancelled,
    )
}

fn node(id: u32, parent: Option<u32>, name: String) -> DirNode {
    DirNode {
        id,
        parent,
        name,
        size: SizeInfo {
            dirs: 1,
            ..SizeInfo::default()
        },
        children: Vec::new(),
        denied: false,
        reparse_skipped: 0,
        cloud_files: 0,
    }
}

fn build_tree(
    root: &Path,
    records: HashMap<u64, Record>,
    elapsed_ms: u64,
    cancelled: bool,
) -> Result<ScanResult, MftError> {
    if !cancelled && records.get(&5).is_none_or(|entry| !entry.directory) {
        return Err(MftError::Invalid);
    }
    let mut children: HashMap<u64, Vec<(u64, &str)>> = HashMap::new();
    for (id, record) in &records {
        if *id < 24 || !record.directory {
            continue;
        }
        for (reference, name) in &record.names {
            if let Some(parent) = parent_record(*reference, &records) {
                children.entry(parent).or_default().push((*id, name));
                break;
            }
        }
    }
    let mut result = ScanResult {
        roots: vec![0],
        nodes: vec![node(0, None, root.to_string_lossy().into_owned())],
        top_files: Vec::new(),
        denied_dirs: 0,
        elapsed_ms,
        cancelled,
    };
    let mut ids = HashMap::from([(5, 0u32)]);
    let mut pending = VecDeque::from([5]);
    while let Some(reference) = pending.pop_front() {
        let parent = ids[&reference];
        for (child, name) in children.remove(&reference).unwrap_or_default() {
            let id = u32::try_from(result.nodes.len()).map_err(|_| MftError::Invalid)?;
            if ids.insert(child, id).is_some() {
                return Err(MftError::Invalid);
            }
            result.nodes.push(node(id, Some(parent), name.to_owned()));
            result.nodes[parent as usize].children.push(id);
            pending.push_back(child);
        }
    }
    let mut top = BinaryHeap::new();
    for (reference, record) in &records {
        if *reference < 24 || record.directory {
            continue;
        }
        for (link, name) in &record.names {
            let Some(parent) = parent_record(*link, &records).and_then(|id| ids.get(&id).copied())
            else {
                continue;
            };
            let size = &mut result.nodes[parent as usize].size;
            size.logical = size.logical.saturating_add(record.logical);
            size.allocated = size.allocated.saturating_add(record.allocated);
            size.files += 1;
            let entry = Reverse((record.allocated, *reference, parent, name));
            if top.len() < 500 {
                top.push(entry);
            } else if top.peek().is_some_and(|smallest| entry < *smallest) {
                top.pop();
                top.push(entry);
            }
            break;
        }
    }
    for child in (1..result.nodes.len()).rev() {
        let size = result.nodes[child].size;
        let parent = result.nodes[child].parent.ok_or(MftError::Invalid)?;
        let aggregate = &mut result.nodes[parent as usize].size;
        aggregate.logical = aggregate.logical.saturating_add(size.logical);
        aggregate.allocated = aggregate.allocated.saturating_add(size.allocated);
        aggregate.files += size.files;
        aggregate.dirs += size.dirs;
    }
    for Reverse((allocated, reference, parent, name)) in top.into_sorted_vec() {
        let record = &records[&reference];
        let path: PathBuf = result.path_of(parent).join(name);
        result.top_files.push(FileEntry {
            path,
            allocated,
            logical: record.logical,
            last_write_time: record.last_write,
        });
    }
    Ok(result)
}

fn parent_record(reference: u64, records: &HashMap<u64, Record>) -> Option<u64> {
    let id = reference & ID_MASK;
    let sequence = (reference >> 48) as u16;
    let parent = records.get(&id)?;
    (parent.directory && parent.sequence == sequence).then_some(id)
}
