#![windows_subsystem = "windows"]
//! 7DaysBackup — 七日杀存档自动备份工具（Rust 原生版）
//! - 单实例（Windows 命名互斥量）
//! - 轮转日志 / 原子配置 / 注册表自启 / 托盘 / egui 界面 / 定时调度

mod app_shared;
mod config;
mod engine;
mod gui;
mod logger;
mod scheduler;
mod startup;
mod tray;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

fn main() {
    let config_dir = config::default_config_dir();
    logger::init(&config_dir);

    // 单实例锁（进程退出自动释放，无 PID 复用误判）
    if !acquire_single_instance() {
        logger::error("已有实例正在运行，退出");
        let _ = rfd::MessageDialog::new()
            .set_title("7 Days Backup")
            .set_level(rfd::MessageLevel::Error)
            .set_description("程序已在运行中，请检查系统托盘。")
            .show();
        return;
    }

    let config = Arc::new(Mutex::new(config::Config::load(&config_dir)));
    let shared = Arc::new(app_shared::Shared::new());
    let engine = Arc::new(Mutex::new(engine::Engine::new(
        config.clone(),
        config_dir.clone(),
        shared.clone(),
    )));

    // 清理上次异常退出残留的临时文件
    engine.lock().unwrap().cleanup_tmp_files();

    // 开机自启：配置开启但未启用时自动创建
    if config.lock().unwrap().auto_start && !startup::is_enabled() {
        let _ = startup::enable();
    }

    // 调度器 + 托盘（托盘菜单自处理，退出不依赖 GUI 线程）
    let _scheduler = scheduler::spawn(config.clone(), engine.clone(), shared.clone());
    let tray = tray::spawn(config.clone(), engine.clone(), shared.clone());

    // 启动 3 秒后执行首次备份（走手动通道，与定时备份共用引擎锁）
    {
        let engine = engine.clone();
        let shared = shared.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(3));
            if !shared.is_exit() && !shared.paused.load(Ordering::Relaxed) {
                scheduler::trigger_manual(&engine, &shared);
            }
        });
    }

    let hidden = std::env::args().any(|a| a == "--hidden");
    logger::info("初始化完成，进入主循环");
    let result = gui::run(config, engine, shared.clone(), config_dir, hidden);

    // 清理退出
    shared.exit.store(true, Ordering::Relaxed);
    tray.stop();
    if let Err(e) = result {
        logger::error(&format!("主循环异常退出: {e}"));
    }
    logger::info("程序已退出");
}

fn acquire_single_instance() -> bool {
    use windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Threading::CreateMutexW;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    unsafe {
        let name = wide("Local\\7DaysBackup_SingleInstance");
        let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if handle.is_null() {
            logger::warn("创建实例互斥量失败，跳过单实例检测");
            return true; // 保守放行，避免因检测失败无法启动
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return false;
        }
        std::mem::forget(handle); // 进程存活期间保持有效，退出由系统回收
        true
    }
}
