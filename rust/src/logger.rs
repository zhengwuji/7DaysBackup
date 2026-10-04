//! 简单的轮转文件日志：backup.log 超过 1MB 时轮转为 .1/.2/.3

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

struct LogState {
    dir: PathBuf,
    start: Instant,
}

static STATE: Mutex<Option<LogState>> = Mutex::new(None);

const MAX_BYTES: u64 = 1024 * 1024;
const KEEP: u32 = 3;

pub fn init(dir: &std::path::Path) {
    let _ = fs::create_dir_all(dir);
    *STATE.lock().unwrap() = Some(LogState {
        dir: dir.to_path_buf(),
        start: Instant::now(),
    });
    info(&format!(
        "==================================================\n7 Days Backup (Rust) v{} 启动 (PID: {})",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    ));
}

fn log_path(dir: &std::path::Path) -> PathBuf {
    dir.join("backup.log")
}

fn rotate(dir: &std::path::Path) {
    let path = log_path(dir);
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() < MAX_BYTES {
            return;
        }
        // .2 -> .3, .1 -> .2, log -> .1
        for i in (1..KEEP).rev() {
            let _ = fs::rename(log_path_with_idx(dir, i), log_path_with_idx(dir, i + 1));
        }
        let _ = fs::rename(&path, log_path_with_idx(dir, 1));
    }
}

fn log_path_with_idx(dir: &std::path::Path, idx: u32) -> PathBuf {
    dir.join(format!("backup.log.{idx}"))
}

pub fn write(level: &str, msg: &str) {
    let guard = STATE.lock().unwrap();
    let Some(state) = guard.as_ref() else { return };
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("[{ts}] [{level}] {msg}");
    rotate(&state.dir);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log_path(&state.dir)) {
        let _ = writeln!(f, "{line}");
    }
    if level == "ERROR" || level == "WARN" {
        eprintln!("{line}");
    }
}

pub fn info(msg: &str) {
    write("INFO", msg);
}

pub fn warn(msg: &str) {
    write("WARN", msg);
}

pub fn error(msg: &str) {
    write("ERROR", msg);
}

pub fn uptime() -> std::time::Duration {
    let guard = STATE.lock().unwrap();
    guard.as_ref().map(|s| s.start.elapsed()).unwrap_or_default()
}
