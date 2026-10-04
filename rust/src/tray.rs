//! 系统托盘：独立线程创建图标并泵 Win32 消息
//! 托盘菜单在本线程直接处理（不依赖 GUI 线程）——
//! 窗口隐藏到托盘后 Windows 不再派发重绘消息，egui 界面循环停摆，
//! 若菜单事件仍交给 GUI 处理，"退出/立即备份"等将全部失效。
//! - 显示设置：直接 ShowWindow 恢复窗口 + 通知 GUI 聚焦
//! - 立即备份：直接触发引擎（共用锁，防并发）
//! - 暂停备份：直接改调度器共享标志
//! - 退出：移除图标后直接结束进程（临时文件下次启动自动清理）

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::app_shared::Shared;
use crate::config::Config;
use crate::engine::Engine;
use crate::gui::WINDOW_TITLE;
use crate::logger;
use crate::scheduler;

pub struct TrayThread {
    handle: std::thread::JoinHandle<()>,
}

impl TrayThread {
    /// 正常情况下退出由托盘线程直接 process::exit 完成；
    /// 此 join 仅作为兜底（GUI 自行退出后主线程调用）。
    pub fn stop(self) {
        let _ = self.handle.join();
    }
}

pub fn spawn(
    config: Arc<Mutex<Config>>,
    engine: Arc<Mutex<Engine>>,
    shared: Arc<Shared>,
) -> TrayThread {
    let handle = std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || run_tray(config, engine, shared))
        .expect("启动托盘线程失败");
    TrayThread { handle }
}

fn run_tray(config: Arc<Mutex<Config>>, engine: Arc<Mutex<Engine>>, shared: Arc<Shared>) {
    if !wait_tray_ready(15) {
        logger::warn("等待系统托盘超时，仍尝试创建");
    }

    let menu = Menu::new();
    let show_item = MenuItem::new("显示设置", true, None);
    let backup_item = MenuItem::new("立即备份", true, None);
    let pause_item = CheckMenuItem::new("暂停备份", true, false, None);
    let open_item = MenuItem::new("打开备份文件夹", true, None);
    let exit_item = MenuItem::new("退出", true, None);
    let _ = menu.append(&show_item);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&backup_item);
    let _ = menu.append(&pause_item);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&open_item);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&exit_item);

    *shared.menu_ids.lock().unwrap() = crate::app_shared::MenuIds {
        show: Some(show_item.id().clone()),
        backup: Some(backup_item.id().clone()),
        pause: Some(pause_item.id().clone()),
        open_folder: Some(open_item.id().clone()),
        exit: Some(exit_item.id().clone()),
    };

    let tray = match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("7DaysBackup")
        .with_menu_on_left_click(true)
        .with_icon(load_icon())
        .build()
    {
        Ok(t) => t,
        Err(e) => {
            logger::error(&format!("创建托盘图标失败: {e}"));
            return;
        }
    };
    logger::info("托盘图标已创建");

    let mut last_tooltip = String::new();
    loop {
        if shared.is_exit() {
            break;
        }
        unsafe { pump_messages() };

        // ---- 菜单事件：本线程直接处理 ----
        for ev in MenuEvent::receiver().try_iter() {
            if ev.id == *show_item.id() {
                restore_app_window();
                shared.show_requested.store(true, Ordering::Relaxed);
            } else if ev.id == *backup_item.id() {
                scheduler::trigger_manual(&engine, &shared);
            } else if ev.id == *pause_item.id() {
                let now = !shared.paused.load(Ordering::Relaxed);
                shared.paused.store(now, Ordering::Relaxed);
                logger::info(if now { "调度器已暂停" } else { "调度器已恢复" });
            } else if ev.id == *open_item.id() {
                open_backup_folder(&config);
            } else if ev.id == *exit_item.id() {
                logger::info("用户通过托盘请求退出");
                // 跳出循环：移除托盘图标后直接结束进程。
                // 备份中途退出残留的临时文件会在下次启动时自动清理。
                std::process::exit(0);
            }
        }

        // 暂停勾选状态与调度器同步
        let paused = shared.paused.load(Ordering::Relaxed);
        if pause_item.is_checked() != paused {
            pause_item.set_checked(paused);
        }

        // 悬停提示
        let desired = format!(
            "7DaysBackup - {} | 下次: {}",
            shared.status_text(),
            shared.next_backup_text()
        );
        if desired != last_tooltip {
            if tray.set_tooltip(Some(desired.as_str())).is_ok() {
                last_tooltip = desired;
            }
        }

        // 气泡通知（备份/恢复失败等）
        for (title, body) in shared.take_notifications() {
            show_notification(&title, &body);
        }

        std::thread::sleep(Duration::from_millis(80));
    }
    drop(tray); // 显式移除托盘图标
    logger::info("托盘图标已移除");
}

fn restore_app_window() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE};
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    unsafe {
        let title = wide(WINDOW_TITLE);
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if !hwnd.is_null() {
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        }
    }
}

fn open_backup_folder(config: &Arc<Mutex<Config>>) {
    let cfg = config.lock().unwrap().clone();
    let custom = cfg.backup_output_path.trim().to_string();
    let dir = if !custom.is_empty() && std::path::Path::new(&custom).is_dir() {
        std::path::PathBuf::from(custom)
    } else {
        cfg.save_path()
    };
    if dir.is_dir() {
        let _ = std::process::Command::new("explorer").arg(&dir).spawn();
    }
}

unsafe fn pump_messages() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, MSG, PM_REMOVE, TranslateMessage,
    };
    let mut msg: MSG = std::mem::zeroed();
    while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) > 0 {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

fn wait_tray_ready(timeout_secs: u64) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    let class = wide("Shell_TrayWnd");
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(timeout_secs) {
        let hwnd = unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) };
        if !hwnd.is_null() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// 加载托盘图标（编译期内嵌 resources/icon.ico，缩放到 32x32）
fn load_icon() -> Icon {
    const ICO: &[u8] = include_bytes!("../../resources/icon.ico");
    let fallback = || {
        let img = image::RgbaImage::from_pixel(16, 16, image::Rgba([46, 125, 50, 255]));
        Icon::from_rgba(img.into_raw(), 16, 16).expect("生成后备图标失败")
    };
    let Ok(img) = image::load_from_memory(ICO) else {
        return fallback();
    };
    let resized = image::imageops::resize(&img.to_rgba8(), 32, 32, image::imageops::FilterType::Lanczos3);
    match Icon::from_rgba(resized.into_raw(), 32, 32) {
        Ok(icon) => icon,
        Err(_) => fallback(),
    }
}

fn show_notification(title: &str, body: &str) {
    let _ = notify_rust::Notification::new()
        .summary(title)
        .body(body)
        .timeout(notify_rust::Timeout::Milliseconds(8000))
        .show();
}
