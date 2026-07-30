//! 后台常驻: start / stop + 单实例互斥 + 探活 + 日志封顶。
//!
//! 探活靠 pid 文件的 flock: 锁由内核在进程退出时释放, 不受 pid 复用与残留 pid 文件影响。

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::provider::{self, Surface};
use crate::store::config_dir;

/// 后台模式的日志目标; 由 `start` 传给子进程。
pub const LOG_ENV: &str = "JJ_PROXY_LOG_FILE";
/// pid 文件里的就绪标记。
const READY: &str = "ready";
/// 单个日志文件上限, 满则轮转一份 -> 磁盘占用恒定 <= 2 倍, 不随运行时长增长。
const LOG_CAP: u64 = 8 * 1024 * 1024;
/// SIGTERM 后等优雅退出的上限: 活跃 SSE 长连接可能一直挂着。
const STOP_WAIT: Duration = Duration::from_secs(5);
/// SIGKILL 后等进程消失的上限。
const KILL_WAIT: Duration = Duration::from_secs(2);
/// 等后台进程写下就绪标记的上限。
const READY_WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(100);

pub fn pid_path() -> PathBuf {
    config_dir().join("daemon.pid")
}

pub fn log_path() -> PathBuf {
    config_dir().join("daemon.log")
}

/// 后台进程入口占位: 拿到独占锁才算本机唯一实例。返回值须活到进程退出。
pub fn acquire() -> Result<File> {
    let mut f = open_pid(true)?;
    if fs4::FileExt::try_lock(&f).is_err() {
        bail!(
            "已在运行{}; 先执行 `jj-agentic-proxy stop`",
            pid_hint(&read_raw(&mut f))
        );
    }
    write_state(&mut f, false)?;
    Ok(f)
}

/// 端口全部监听成功后回写就绪标记 -> `start` 能确定性确认启动成功, 而非靠「端口通」(可被他人占用冒充)。
pub fn mark_ready(f: &mut File) -> Result<()> {
    write_state(f, true)
}

/// `None` = 无实例在跑 (pid 文件残留也算)。
pub fn running() -> Result<Option<u32>> {
    Ok(state()?.map(|(pid, _)| pid))
}

/// 脱离终端启动, 等就绪标记后才报成功。
pub fn start() -> Result<()> {
    if let Some(pid) = running()? {
        println!("已在运行 (pid {pid})");
        return Ok(());
    }
    let exe = std::env::current_exe().context("定位自身可执行文件失败")?;
    let log = log_path();
    fs::create_dir_all(config_dir())?;
    // stdout/stderr 同落日志文件: tracing 之外的 panic 输出不能丢。
    let out = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("打开日志失败: {}", log.display()))?;
    let err = out.try_clone()?;

    let mut cmd = Command::new(&exe);
    cmd.arg("serve")
        .env(LOG_ENV, &log)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    // 新会话 -> 终端关闭 / Ctrl-C 不再波及。
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("启动 {} 失败", exe.display()))?;

    let deadline = Instant::now() + READY_WAIT;
    loop {
        if let Some(st) = child.try_wait()? {
            bail!("启动失败 ({st}):\n{}", tail(&log));
        }
        if state()? == Some((child.id(), true)) {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            bail!("启动超时:\n{}", tail(&log));
        }
        std::thread::sleep(POLL);
    }

    println!("已启动 (pid {})", child.id());
    for s in Surface::ALL {
        println!("- {s:<13} http://{}:{}", provider::HOST, s.port());
    }
    println!("日志 {}", log.display());
    Ok(())
}

/// SIGTERM -> 等锁释放 (= 进程真的退出) -> 兜底 SIGKILL。
pub fn stop() -> Result<()> {
    let Some(pid) = running()? else {
        println!("未在运行");
        return Ok(());
    };
    signal(pid, libc::SIGTERM)?;
    if wait_gone(STOP_WAIT)? {
        println!("已停止 (pid {pid})");
        return Ok(());
    }
    println!("优雅退出超时 (pid {pid}), 强制结束");
    signal(pid, libc::SIGKILL)?;
    if wait_gone(KILL_WAIT)? {
        println!("已停止 (pid {pid})");
        return Ok(());
    }
    bail!("停止失败: pid {pid} 仍在运行")
}

/// 活着的实例: `(pid, 是否已监听)`。
fn state() -> Result<Option<(u32, bool)>> {
    let mut f = match open_pid(false) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("打开 {} 失败", pid_path().display())),
    };
    if fs4::FileExt::try_lock(&f).is_ok() {
        let _ = fs4::FileExt::unlock(&f);
        return Ok(None);
    }
    let raw = read_raw(&mut f);
    Ok(Some((parse_pid(&raw).unwrap_or(0), raw.contains(READY))))
}

fn open_pid(create: bool) -> io::Result<File> {
    if create {
        fs::create_dir_all(config_dir())?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .open(pid_path())
}

fn write_state(f: &mut File, ready: bool) -> Result<()> {
    f.set_len(0)?;
    f.seek(SeekFrom::Start(0))?;
    let pid = std::process::id();
    if ready {
        writeln!(f, "{pid} {READY}")?;
    } else {
        writeln!(f, "{pid}")?;
    }
    f.flush()?;
    Ok(())
}

fn read_raw(f: &mut File) -> String {
    let mut s = String::new();
    if f.seek(SeekFrom::Start(0)).is_ok() {
        let _ = f.read_to_string(&mut s);
    }
    s
}

fn parse_pid(raw: &str) -> Option<u32> {
    raw.split_whitespace().next()?.parse().ok()
}

fn pid_hint(raw: &str) -> String {
    parse_pid(raw)
        .map(|p| format!(" (pid {p})"))
        .unwrap_or_default()
}

fn wait_gone(limit: Duration) -> Result<bool> {
    let deadline = Instant::now() + limit;
    loop {
        if running()?.is_none() {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(POLL);
    }
}

fn signal(pid: u32, sig: libc::c_int) -> Result<()> {
    // SAFETY: 目标 pid 来自本机 pid 文件, 且该进程正持有文件锁 = 本程序自身。
    if unsafe { libc::kill(pid as libc::pid_t, sig) } == 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    // 已经不在了: 与目标状态一致, 不算失败。
    if e.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(e).with_context(|| format!("向 pid {pid} 发信号失败"))
}

/// 启动失败时的诊断上下文: 日志末尾若干行。
fn tail(path: &Path) -> String {
    const LINES: usize = 20;
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(LINES);
    lines[from..].join("\n")
}

/// 后台日志: 写满即轮转一份, 磁盘占用不随时间增长。
pub struct CappedLog {
    path: PathBuf,
    file: File,
    written: u64,
}

impl CappedLog {
    pub fn open(path: PathBuf) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            path,
            file,
            written,
        })
    }

    fn rotate(&mut self) -> io::Result<()> {
        let mut old = OsString::from(self.path.clone());
        old.push(".1");
        fs::rename(&self.path, PathBuf::from(old))?;
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.written = 0;
        Ok(())
    }
}

impl io::Write for CappedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written + buf.len() as u64 > LOG_CAP {
            self.rotate()?;
        }
        let n = self.file.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capped_log_rotates_and_keeps_one_backup() {
        let dir = std::env::temp_dir().join(format!("jj-proxy-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("daemon.log");

        let mut log = CappedLog::open(path.clone()).unwrap();
        log.written = LOG_CAP; // 下一次写入必然触发轮转
        log.write_all(b"fresh\n").unwrap();
        log.flush().unwrap();

        assert!(path.with_file_name("daemon.log.1").exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), "fresh\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_line_carries_pid_and_ready_flag() {
        assert_eq!(parse_pid("4321 ready\n"), Some(4321));
        assert_eq!(parse_pid("4321\n"), Some(4321));
        assert_eq!(parse_pid(""), None);
        assert!("4321 ready\n".contains(READY));
        assert!(!"4321\n".contains(READY));
    }
}
