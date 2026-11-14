//! 外部命令的查找与启动。
//!
//! 全项目只有这里可以启动外部进程。命令失败或超时都只报告结果：
//! 超时只停止等待，不结束进程；这里从不删除任何路径。

use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::win::{KnownFolder, known_folder};

/// 不显示控制台窗口。npm 这类 `.cmd` 脚本会自己开窗口，必须显式关掉。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 每路输出保留的末尾字节数。命令输出可能很长，界面只需要最后一段判断原因。
const TAIL_BYTES: usize = 4096;

/// 等待子进程时的轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 查找工具时依次尝试的扩展名。`.ps1` 不尝试：执行策略常常禁用它。
const TOOL_EXTENSIONS: &[&str] = &["exe", "cmd", "bat"];

/// 一次命令执行的结果。
pub struct CmdOutcome {
    /// 退出码。启动失败时为 None。
    pub exit_code: Option<i32>,
    /// 等待超时。超时后进程仍在运行，这里只停止等待。
    pub timed_out: bool,
    pub stdout_tail: String,
    pub stderr_tail: String,
}

/// 在资源管理器中打开并选中目标。
///
/// 不等待 explorer 退出：它会把命令行转交给已经运行的实例后马上返回。
pub fn open_in_explorer(path: &Path) -> io::Result<()> {
    let explorer = known_folder(KnownFolder::Windows)
        .map_err(io::Error::other)?
        .join("explorer.exe");

    // 用 raw_arg 原样拼参数：/select 后面的引号必须由我们自己控制，
    // 交给 std 的默认转义会改掉 explorer 看到的命令行。
    let argument = format!("/select,\"{}\"", path.display());
    std::process::Command::new(explorer)
        .raw_arg(argument)
        .spawn()?;

    Ok(())
}

/// 按 PATH 中目录的顺序查找工具，返回第一个命中的完整路径。
///
/// 同一个目录里依次尝试 `.exe`、`.cmd`、`.bat`：Windows 上同名文件可能有多个，
/// 顺序固定才能保证同一台机器上每次找到的是同一个。
pub fn resolve_tool_in(name: &str, path_var: &OsStr) -> Option<PathBuf> {
    for dir in std::env::split_paths(path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }

        for extension in TOOL_EXTENSIONS {
            let mut file = OsString::from(name);
            file.push(".");
            file.push(extension);

            let candidate = dir.join(&file);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// 用当前进程的 PATH 查找工具。
pub fn resolve_tool(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;

    resolve_tool_in(name, &path_var)
}

/// 启动命令并等待它结束，最多等 `timeout`。
///
/// 超时只报告，不结束进程：强杀别的工具的进程可能留下半个缓存，也可能打断它自己的清理。
pub fn run(tool: &Path, args: &[&str], timeout: Duration) -> CmdOutcome {
    run_with_env(tool, args, timeout, &[])
}

/// 同 [`run`]，但先覆盖几个环境变量再启动。
///
/// 子进程继承的是本进程的环境，这里只覆盖传入的变量：迁移验证要的正是
/// “当前环境 + 新变量”，不能清空环境另建一套，否则 PATH 之类的会丢。
pub fn run_with_env(
    tool: &Path,
    args: &[&str],
    timeout: Duration,
    env: &[(&str, String)],
) -> CmdOutcome {
    let mut command = match command_for(tool) {
        Ok(command) => command,
        Err(error) => return failed(&error),
    };

    // 参数只有内置常量，std 默认的转义在这里足够；stdin 给 null，
    // 免得不小心读到界面进程的输入而卡住。
    command
        .args(args)
        .envs(env.iter().map(|(name, value)| (*name, value.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return failed(&format!("无法启动命令：{error}")),
    };

    // 输出必须边跑边读：等进程结束再读管道，输出多时双方会互相等死。
    let stdout = Tail::drain(child.stdout.take());
    let stderr = Tail::drain(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let mut exit_code = None;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                break;
            }
            Ok(None) => {}
            Err(error) => {
                return CmdOutcome {
                    exit_code: None,
                    timed_out: false,
                    stdout_tail: stdout.snapshot(),
                    stderr_tail: format!("等待命令结束时出错：{error}"),
                };
            }
        }

        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }

        std::thread::sleep(POLL_INTERVAL);
    }

    CmdOutcome {
        exit_code,
        timed_out,
        stdout_tail: stdout.snapshot(),
        stderr_tail: stderr.snapshot(),
    }
}

/// 启动命令并等待结束，同时把标准输出的新内容交给调用方处理。
///
/// DISM 这类长耗时命令会在运行中不断刷新进度行，只有边跑边收才拿得到百分比；
/// [`run`] 只在命令结束后给出末尾输出，做不到这一点。
///
/// 和 [`run`] 一样：超时只停止等待，不结束进程。
pub fn run_streaming(
    tool: &Path,
    args: &[&str],
    timeout: Duration,
    on_stdout: &mut dyn FnMut(&str),
) -> CmdOutcome {
    let mut command = match command_for(tool) {
        Ok(command) => command,
        Err(error) => return failed(&error),
    };

    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return failed(&format!("无法启动命令：{error}")),
    };

    // 标准输出走通道回到本线程：读取线程只负责读，调用方的回调在本线程里执行。
    let (sender, receiver) = std::sync::mpsc::channel::<String>();
    drain_to_channel(child.stdout.take(), sender);
    let stderr = Tail::drain(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let mut stdout_tail = String::new();
    let mut timed_out = false;
    let mut exit_code = None;

    loop {
        // 先收输出再判退出：进程退出前写入的最后几行还在管道里。
        match receiver.recv_timeout(POLL_INTERVAL) {
            Ok(chunk) => {
                push_tail(&mut stdout_tail, &chunk);
                on_stdout(&chunk);
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            // 管道已关闭但进程还没退出，稍等一下再查，避免空转。
            Err(RecvTimeoutError::Disconnected) => std::thread::sleep(POLL_INTERVAL),
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                break;
            }
            Ok(None) => {}
            Err(error) => {
                return CmdOutcome {
                    exit_code: None,
                    timed_out: false,
                    stdout_tail,
                    stderr_tail: format!("等待命令结束时出错：{error}"),
                };
            }
        }

        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
    }

    // 退出之后管道里可能还有没读完的行，这里再收一轮。
    while let Ok(chunk) = receiver.recv_timeout(POLL_INTERVAL) {
        push_tail(&mut stdout_tail, &chunk);
        on_stdout(&chunk);
    }

    CmdOutcome {
        exit_code,
        timed_out,
        stdout_tail,
        stderr_tail: stderr.snapshot(),
    }
}

/// 只保留末尾 `TAIL_BYTES` 长度的输出。
fn push_tail(buffer: &mut String, chunk: &str) {
    buffer.push_str(chunk);
    if buffer.len() <= TAIL_BYTES * 2 {
        return;
    }

    let mut cut = buffer.len() - TAIL_BYTES;
    // 从字符边界切开，避免把多字节字符截成半个。
    while cut < buffer.len() && !buffer.is_char_boundary(cut) {
        cut += 1;
    }
    buffer.drain(..cut);
}

/// 把标准输出的新内容按块送到主线程。管道关闭时线程自己结束。
fn drain_to_channel(pipe: Option<impl Read + Send + 'static>, sender: Sender<String>) {
    let Some(mut pipe) = pipe else {
        return;
    };

    std::thread::spawn(move || {
        let mut chunk = [0u8; 1024];
        loop {
            let read = match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => break,
            };

            // 接收端已经不在了（调用方超时返回），没必要继续读。
            let text = String::from_utf8_lossy(&chunk[..read]).into_owned();
            if sender.send(text).is_err() {
                break;
            }
        }
    });
}

/// 组装要执行的命令。`.cmd` 和 `.bat` 是批处理脚本，必须交给 cmd.exe 解释。
fn command_for(tool: &Path) -> Result<Command, String> {
    if !needs_cmd_host(tool) {
        return Ok(Command::new(tool));
    }

    let cmd = known_folder(KnownFolder::Windows)
        .map_err(|error| format!("无法定位系统目录：{error}"))?
        .join("System32")
        .join("cmd.exe");

    let mut command = Command::new(cmd);
    // 完整路径带空格时 std 会给它加一对引号；cmd 看到命令行里正好一对引号、
    // 引号内是带空格的可执行文件名，会保留引号并直接执行，这是它文档里写明的规则。
    command.arg("/d").arg("/c").arg(tool);

    Ok(command)
}

/// 是否是必须在 cmd.exe 里执行的批处理脚本。
fn needs_cmd_host(tool: &Path) -> bool {
    match tool.extension().and_then(OsStr::to_str) {
        Some(extension) => {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        }
        None => false,
    }
}

/// 启动失败时的结果：原因放在 stderr 里，调用方按“命令失败”报告。
fn failed(message: &str) -> CmdOutcome {
    CmdOutcome {
        exit_code: None,
        timed_out: false,
        stdout_tail: String::new(),
        stderr_tail: message.to_string(),
    }
}

/// 一个输出管道的读取线程，只保留末尾若干字节。
///
/// 超时返回时读取线程还在跑：进程没结束，管道就不会关闭，线程会自己退出。
struct Tail {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl Tail {
    fn drain(pipe: Option<impl Read + Send + 'static>) -> Self {
        let buffer = Arc::new(Mutex::new(Vec::new()));

        let Some(mut pipe) = pipe else {
            return Self { buffer };
        };

        let shared = Arc::clone(&buffer);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 1024];
            loop {
                let read = match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(_) => break,
                };

                let mut guard = shared.lock().unwrap();
                guard.extend_from_slice(&chunk[..read]);
                // 保留两倍长度再裁剪，避免每个块都搬一次内存。
                if guard.len() > TAIL_BYTES * 2 {
                    let cut = guard.len() - TAIL_BYTES;
                    guard.drain(..cut);
                }
            }
        });

        Self { buffer }
    }

    /// 当前已经读到的末尾内容。超时返回时可能只有前半段。
    fn snapshot(&self) -> String {
        let guard = self.buffer.lock().unwrap();
        let start = guard.len().saturating_sub(TAIL_BYTES);

        String::from_utf8_lossy(&guard[start..]).into_owned()
    }
}
