//! 并行目录扫描器。

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN,
};
use windows::Win32::System::SystemServices::{
    IO_REPARSE_TAG_CLOUD, IO_REPARSE_TAG_CLOUD_MASK, IO_REPARSE_TAG_DEDUP,
};

use crate::error::{CoreError, Result};
use crate::model::{FileIdentity, SizeInfo};
use crate::win::dirinfo;

pub use crate::win::dirinfo::EntryInfo;

/// 硬链接去重集合的分片数：分片加锁，避免所有线程抢同一把锁。
const HARD_LINK_SHARDS: usize = 64;

/// 文件过滤器：返回 false 的文件不计入任何统计，规则子集可以复用它统计大小。
pub type FileFilter = Arc<dyn Fn(&EntryInfo) -> bool + Send + Sync>;

/// 扫描选项。
pub struct ScanOptions {
    pub threads: usize,
    pub top_files: usize,
    pub file_filter: Option<FileFilter>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        // 机械盘和 USB 盘上线程太多只会互相抢 IO，最多开到 8 个。
        let cpus = std::thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1);
        Self {
            threads: cpus.min(8),
            top_files: 500,
            file_filter: None,
        }
    }
}

/// 扫描进度，供界面每隔一段时间读取一次。
#[derive(Debug, Default)]
pub struct ScanProgress {
    pub dirs: AtomicU64,
    pub files: AtomicU64,
    pub allocated: AtomicU64,
}

/// 目录树节点。`size` 含整个子树。
pub struct DirNode {
    pub id: u32,
    pub parent: Option<u32>,
    pub name: String,
    pub size: SizeInfo,
    pub children: Vec<u32>,
    pub denied: bool,
    pub reparse_skipped: u32,
    pub cloud_files: u32,
}

/// Top N 文件。
pub struct FileEntry {
    pub path: PathBuf,
    pub logical: u64,
    pub allocated: u64,
    pub last_write_time: u64,
}

/// 一次扫描的结果。
pub struct ScanResult {
    pub roots: Vec<u32>,
    pub nodes: Vec<DirNode>,
    pub top_files: Vec<FileEntry>,
    pub denied_dirs: u64,
    pub elapsed_ms: u64,
    pub cancelled: bool,
}

impl ScanResult {
    /// 按节点 ID 还原完整路径。根节点保存的就是调用方传入的路径。
    pub fn path_of(&self, id: u32) -> PathBuf {
        let mut names: Vec<&str> = Vec::new();
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = &self.nodes[node_id as usize];
            names.push(&node.name);
            current = node.parent;
        }

        // names 是从目标往上收集的，最后一个是根节点，它本身就是完整路径。
        let mut path = PathBuf::new();
        for name in names.iter().rev() {
            path.push(name);
        }
        path
    }

    /// 子目录按占盘大小降序。
    pub fn children_by_size(&self, id: u32) -> Vec<&DirNode> {
        let mut children: Vec<&DirNode> = self.nodes[id as usize]
            .children
            .iter()
            .map(|child_id| &self.nodes[*child_id as usize])
            .collect();
        children.sort_by_key(|child| std::cmp::Reverse(child.size.allocated));
        children
    }
}

/// 扫描若干根目录。取消时返回已经扫到的部分结果。
pub fn scan(
    roots: &[PathBuf],
    opts: &ScanOptions,
    progress: &ScanProgress,
    cancel: &AtomicBool,
) -> Result<ScanResult> {
    let started = Instant::now();
    let threads = opts.threads.max(1);
    let context = ScanContext {
        progress,
        cancel,
        options: opts,
        seen: HardLinkSet::new(HARD_LINK_SHARDS),
        top_files: TopFilesCollector::new(threads, opts.top_files),
    };

    // 线程数由任务书要求决定，所以自建线程池而不是用全局池。
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|error| CoreError::ScanStart(error.to_string()))?;

    let scanned: Vec<DirScan> = pool.install(|| {
        let collected: Mutex<Vec<(usize, DirScan)>> = Mutex::new(Vec::new());
        rayon::scope(|scope| {
            for (index, root) in roots.iter().enumerate() {
                let collected = &collected;
                let context = &context;
                scope.spawn(move |_| {
                    // 根节点保存调用方传入的路径，路径还原时靠它拼出完整路径。
                    let name = root.to_string_lossy().into_owned();
                    if let Some(node) = scan_dir(root, name, context) {
                        collected.lock().unwrap().push((index, node));
                    }
                });
            }
        });

        let mut collected = collected.into_inner().unwrap();
        collected.sort_by_key(|(index, _)| *index);
        collected.into_iter().map(|(_, node)| node).collect()
    });

    let mut nodes: Vec<DirNode> = Vec::new();
    let mut root_ids = Vec::with_capacity(scanned.len());
    for node in scanned {
        root_ids.push(flatten(node, None, &mut nodes));
    }

    let denied_dirs = nodes.iter().filter(|node| node.denied).count() as u64;

    Ok(ScanResult {
        roots: root_ids,
        nodes,
        top_files: context.top_files.finish(),
        denied_dirs,
        elapsed_ms: started.elapsed().as_millis() as u64,
        cancelled: cancel.load(Ordering::Relaxed),
    })
}

/// 扫描一个目录，返回它的中间结果。被取消时返回 None。
fn scan_dir(path: &Path, name: String, ctx: &ScanContext<'_>) -> Option<DirScan> {
    if ctx.cancel.load(Ordering::Relaxed) {
        return None;
    }
    ctx.progress.dirs.fetch_add(1, Ordering::Relaxed);

    let mut node = DirScan {
        name,
        size: SizeInfo {
            dirs: 1,
            ..SizeInfo::default()
        },
        children: Vec::new(),
        denied: false,
        reparse_skipped: 0,
        cloud_files: 0,
    };

    let handle = match dirinfo::open_dir(path) {
        Ok(handle) => handle,
        Err(_) => {
            // 打不开就标记为“无权读取”，继续扫描其他目录。
            node.denied = true;
            return Some(node);
        }
    };

    let volume_serial = dirinfo::volume_serial(&handle).unwrap_or(0);
    let mut own_size = SizeInfo::default();
    let mut subdirs: Vec<(String, PathBuf)> = Vec::new();

    let listed = dirinfo::for_each_entry(&handle, |entry| {
        let entry_path = path.join(&entry.name);

        if entry.is_dir {
            if entry.is_reparse() {
                // junction 和符号链接目录不进入，否则会重复统计甚至绕到别的卷。
                node.reparse_skipped += 1;
                return;
            }
            subdirs.push((entry.name.to_string_lossy().into_owned(), entry_path));
            return;
        }

        let count = classify_file(&entry);
        if count == FileCount::Skip {
            return;
        }

        if let Some(filter) = &ctx.options.file_filter
            && !filter(&entry)
        {
            return;
        }

        let identity = FileIdentity {
            volume_serial,
            file_id: entry.file_id,
        };
        if !ctx.seen.insert(identity) {
            // 硬链接的另一个名字：大小和文件数都只算一次。
            return;
        }

        if count == FileCount::AllocatedOnly {
            // 云占位符本地没有数据，逻辑大小没有意义，只计入占盘大小。
            node.cloud_files += 1;
        } else {
            own_size.logical += entry.logical;
        }
        own_size.allocated += entry.allocated;
        own_size.files += 1;

        ctx.progress.files.fetch_add(1, Ordering::Relaxed);
        ctx.progress
            .allocated
            .fetch_add(entry.allocated, Ordering::Relaxed);
        ctx.top_files.push(Candidate {
            path: entry_path,
            logical: entry.logical,
            allocated: entry.allocated,
            last_write_time: entry.last_write_time,
        });
    });

    if listed.is_err() {
        // 枚举到一半失败说明这个目录没读全，按“无权读取”标记。
        node.denied = true;
    }

    // 句柄在递归子目录前释放，避免深层目录树同时占用大量句柄。
    drop(handle);

    if !subdirs.is_empty() {
        let children: Mutex<Vec<(usize, DirScan)>> = Mutex::new(Vec::with_capacity(subdirs.len()));
        rayon::scope(|scope| {
            for (index, (child_name, child_path)) in subdirs.into_iter().enumerate() {
                let children = &children;
                scope.spawn(move |_| {
                    if let Some(child) = scan_dir(&child_path, child_name, ctx) {
                        children.lock().unwrap().push((index, child));
                    }
                });
            }
        });

        // 按枚举顺序还原，结果才和目录里的实际顺序一致。
        let mut collected = children.into_inner().unwrap();
        collected.sort_by_key(|(index, _)| *index);
        node.children = collected.into_iter().map(|(_, child)| child).collect();
    }

    // 子树大小在最后自底向上汇总，扫描过程中不需要改共享数据。
    let mut total = own_size;
    for child in &node.children {
        total.logical += child.size.logical;
        total.allocated += child.size.allocated;
        total.files += child.size.files;
        total.dirs += child.size.dirs;
    }
    // 目录数含自身，这样根节点的 dirs 就是整棵子树的目录总数。
    total.dirs += 1;
    node.size = total;

    Some(node)
}

/// 把中间结果展平成按父在前的数组，节点 ID 就是数组下标。
fn flatten(node: DirScan, parent: Option<u32>, nodes: &mut Vec<DirNode>) -> u32 {
    let id = nodes.len() as u32;
    nodes.push(DirNode {
        id,
        parent,
        name: node.name,
        size: node.size,
        children: Vec::new(),
        denied: node.denied,
        reparse_skipped: node.reparse_skipped,
        cloud_files: node.cloud_files,
    });

    for child in node.children {
        let child_id = flatten(child, Some(id), nodes);
        nodes[id as usize].children.push(child_id);
    }

    id
}

/// 文件在统计里怎么算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileCount {
    /// 符号链接、AppExecLink 之类，不算。
    Skip,
    /// 普通文件和 de-dup：两个口径都算。
    Both,
    /// 云占位符：只算占盘大小。
    AllocatedOnly,
}

/// 规则统计和占用检查也要按同一套口径取舍文件，所以对 crate 内开放。
pub(crate) fn classify_file(entry: &EntryInfo) -> FileCount {
    if !entry.is_reparse() {
        return FileCount::Both;
    }

    if is_cloud_placeholder(entry) {
        return FileCount::AllocatedOnly;
    }

    // 去重卷的文件照常计入，大小口径已经由系统算好。
    if entry.reparse_tag == IO_REPARSE_TAG_DEDUP {
        return FileCount::Both;
    }

    FileCount::Skip
}

fn is_cloud_placeholder(entry: &EntryInfo) -> bool {
    let recall_bits = FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0 | FILE_ATTRIBUTE_RECALL_ON_OPEN.0;
    if entry.attributes & recall_bits != 0 {
        return true;
    }

    // 云标签只有低 12~15 位不同（CLOUD、CLOUD_1 … CLOUD_F），掩码后统一比较。
    entry.reparse_tag & !IO_REPARSE_TAG_CLOUD_MASK == IO_REPARSE_TAG_CLOUD
}

/// 扫描过程中的共享状态。
struct ScanContext<'a> {
    progress: &'a ScanProgress,
    cancel: &'a AtomicBool,
    options: &'a ScanOptions,
    seen: HardLinkSet,
    top_files: TopFilesCollector,
}

/// 中间结果。大小在最后自底向上汇总。
struct DirScan {
    name: String,
    size: SizeInfo,
    children: Vec<DirScan>,
    denied: bool,
    reparse_skipped: u32,
    cloud_files: u32,
}

/// 硬链接去重：分片加锁的集合，记录已经统计过的文件身份。
struct HardLinkSet {
    shards: Vec<Mutex<HashSet<FileIdentity>>>,
}

impl HardLinkSet {
    fn new(shards: usize) -> Self {
        Self {
            shards: (0..shards).map(|_| Mutex::new(HashSet::new())).collect(),
        }
    }

    /// 第一次见到这个文件返回 true，之后同名硬链接返回 false。
    fn insert(&self, identity: FileIdentity) -> bool {
        let mut hasher = DefaultHasher::new();
        identity.hash(&mut hasher);
        let index = (hasher.finish() as usize) % self.shards.len();
        self.shards[index].lock().unwrap().insert(identity)
    }
}

/// Top N 候选。只按占盘大小比较，大小相同时用路径保证结果稳定。
#[derive(PartialEq, Eq)]
struct Candidate {
    path: PathBuf,
    logical: u64,
    allocated: u64,
    last_write_time: u64,
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.allocated
            .cmp(&other.allocated)
            .then_with(|| self.path.cmp(&other.path))
    }
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// 有界最小堆：堆顶是入选文件里最小的那个，比它大才需要换进来。
struct TopFiles {
    limit: usize,
    heap: BinaryHeap<Reverse<Candidate>>,
}

impl TopFiles {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            heap: BinaryHeap::new(),
        }
    }

    fn push(&mut self, candidate: Candidate) {
        if self.limit == 0 {
            return;
        }

        if self.heap.len() < self.limit {
            self.heap.push(Reverse(candidate));
            return;
        }

        if let Some(mut smallest) = self.heap.peek_mut()
            && candidate > smallest.0
        {
            *smallest = Reverse(candidate);
        }
    }
}

/// 每个线程一个有界最小堆，最后合并。
struct TopFilesCollector {
    limit: usize,
    heaps: Vec<Mutex<TopFiles>>,
}

impl TopFilesCollector {
    fn new(threads: usize, limit: usize) -> Self {
        Self {
            limit,
            heaps: (0..threads.max(1))
                .map(|_| Mutex::new(TopFiles::new(limit)))
                .collect(),
        }
    }

    fn push(&self, candidate: Candidate) {
        let index = rayon::current_thread_index().unwrap_or(0) % self.heaps.len();
        self.heaps[index].lock().unwrap().push(candidate);
    }

    /// 合并各线程的堆，按占盘大小降序取前 limit 个。
    fn finish(&self) -> Vec<FileEntry> {
        let mut candidates: Vec<Candidate> = Vec::new();
        for heap in &self.heaps {
            let mut heap = heap.lock().unwrap();
            for Reverse(candidate) in std::mem::take(&mut heap.heap) {
                candidates.push(candidate);
            }
        }

        candidates.sort_by(|left, right| right.cmp(left));
        candidates.truncate(self.limit);
        candidates
            .into_iter()
            .map(|candidate| FileEntry {
                path: candidate.path,
                logical: candidate.logical,
                allocated: candidate.allocated,
                last_write_time: candidate.last_write_time,
            })
            .collect()
    }
}
