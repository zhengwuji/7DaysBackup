//! 定时调度器：独立线程，每 250ms 检查一次
//! - 间隔从配置实时读取（GUI 修改立即生效）
//! - 暂停/恢复由共享标志控制（托盘勾选项）
//! - 手动备份与定时备份共用 Engine 锁（try_lock 防并发，天然串行）

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app_shared::Shared;
use crate::config::Config;
use crate::engine::Engine;

pub fn spawn(
    config: Arc<Mutex<Config>>,
    engine: Arc<Mutex<Engine>>,
    shared: Arc<Shared>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("scheduler".into())
        .spawn(move || {
            logger_init_msg();
            let mut next_due: Option<Instant> = None;
            loop {
                if shared.is_exit() {
                    break;
                }
                if shared.paused.load(Ordering::Relaxed) {
                    next_due = None;
                    shared.set_next_backup("已暂停");
                    std::thread::sleep(Duration::from_millis(250));
                    continue;
                }
                let interval = config.lock().unwrap().backup_interval_seconds.max(5);
                let due = match next_due {
                    Some(d) => d,
                    None => {
                        let d = Instant::now() + Duration::from_secs(interval);
                        shared.set_next_backup(fmt_wall_clock(interval));
                        next_due = Some(d);
                        d
                    }
                };
                let now = Instant::now();
                if now < due {
                    std::thread::sleep((due - now).min(Duration::from_millis(250)));
                    continue;
                }
                shared.set_status("备份中...");
                let status = engine.lock().unwrap().run_backup();
                shared.set_status(status.message.clone());
                shared.dirty_scan.store(true, Ordering::Relaxed);
                next_due = Some(Instant::now() + Duration::from_secs(interval));
                shared.set_next_backup(fmt_wall_clock(interval));
            }
            crate::logger::info("调度器已停止");
        })
        .expect("启动调度器线程失败")
}

fn logger_init_msg() {
    crate::logger::info("调度器已启动");
}

fn fmt_wall_clock(interval_secs: u64) -> String {
    let t = chrono::Local::now() + chrono::Duration::seconds(interval_secs as i64);
    t.format("%H:%M:%S").to_string()
}

/// 手动触发一次备份（GUI 按钮 / 托盘菜单 / 启动后的首次备份）
pub fn trigger_manual(engine: &Arc<Mutex<Engine>>, shared: &Arc<Shared>) {
    let eng = engine.clone();
    let sh = shared.clone();
    let _ = std::thread::Builder::new()
        .name("manual-backup".into())
        .spawn(move || {
            let mut guard = match eng.try_lock() {
                Ok(g) => g,
                Err(_) => {
                    sh.set_status("备份正在进行中，请稍候");
                    return;
                }
            };
            sh.set_status("手动备份中...");
            let status = guard.run_backup();
            sh.set_status(status.message);
            sh.dirty_scan.store(true, Ordering::Relaxed);
        });
}
