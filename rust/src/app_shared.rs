//! 线程间共享状态：托盘/GUI/调度器/引擎之间的轻量通道

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

#[derive(Clone, Default)]
pub struct MenuIds {
    pub show: Option<tray_icon::menu::MenuId>,
    pub backup: Option<tray_icon::menu::MenuId>,
    pub pause: Option<tray_icon::menu::MenuId>,
    pub open_folder: Option<tray_icon::menu::MenuId>,
    pub exit: Option<tray_icon::menu::MenuId>,
}

pub struct Shared {
    /// 退出请求（托盘菜单触发）
    pub exit: AtomicBool,
    /// 显示设置窗口
    pub show_requested: AtomicBool,
    /// 立即备份
    pub backup_now: AtomicBool,
    /// 打开备份文件夹
    pub open_backup_folder: AtomicBool,
    /// 调度器暂停（托盘勾选项 / 预留 GUI 开关）
    pub paused: AtomicBool,
    /// 状态栏文本（"空闲" / "备份中..." / 结果摘要）
    pub status: Mutex<String>,
    /// 下次备份时间文本
    pub next_backup: Mutex<String>,
    /// 待发送的气泡通知队列 (title, body)
    pub notify_queue: Mutex<VecDeque<(String, String)>>,
    /// 托盘菜单 ID（tray 线程写入，GUI 线程读取比对）
    pub menu_ids: Mutex<MenuIds>,
    /// GUI 扫描数据脏标记（有新数据需要重新拉取）
    pub dirty_scan: AtomicBool,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            exit: AtomicBool::new(false),
            show_requested: AtomicBool::new(false),
            backup_now: AtomicBool::new(false),
            open_backup_folder: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            status: Mutex::new("就绪".into()),
            next_backup: Mutex::new("--".into()),
            notify_queue: Mutex::new(VecDeque::new()),
            menu_ids: Mutex::new(MenuIds::default()),
            dirty_scan: AtomicBool::new(true),
        }
    }

    pub fn set_status(&self, text: impl Into<String>) {
        *self.status.lock().unwrap() = text.into();
    }

    pub fn status_text(&self) -> String {
        self.status.lock().unwrap().clone()
    }

    pub fn set_next_backup(&self, text: impl Into<String>) {
        *self.next_backup.lock().unwrap() = text.into();
    }

    pub fn next_backup_text(&self) -> String {
        self.next_backup.lock().unwrap().clone()
    }

    pub fn notify(&self, title: impl Into<String>, body: impl Into<String>) {
        self.notify_queue
            .lock()
            .unwrap()
            .push_back((title.into(), body.into()));
    }

    pub fn take_notifications(&self) -> Vec<(String, String)> {
        self.notify_queue.lock().unwrap().drain(..).collect()
    }

    pub fn is_exit(&self) -> bool {
        self.exit.load(Ordering::Relaxed)
    }
}
