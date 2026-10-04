//! egui 设置界面：存档树（勾选）、备份历史 + 快照列表、恢复确认、设置实时保存
//! 界面语言为中文 —— 启动时加载 Windows 系统中文字体（egui 默认字体无 CJK 字形）

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::app_shared::Shared;
use crate::config::Config;
use crate::engine::{BackupEntry, Engine, SaveInfo};
use crate::logger;
use crate::scheduler;
use crate::startup;

const SCAN_INTERVAL: Duration = Duration::from_secs(15);

pub const WINDOW_TITLE: &str = "7 Days to Die 存档备份工具";

const ACCENT: egui::Color32 = egui::Color32::from_rgb(46, 125, 50);
const DANGER: egui::Color32 = egui::Color32::from_rgb(198, 40, 40);

// ===================== 字体与样式 =====================

/// 加载 Windows 系统中文字体（微软雅黑 → 黑体 → 宋体逐级回退），修复中文显示为方框的问题
fn install_fonts(ctx: &egui::Context) {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let candidates = [
        format!(r"{windir}\Fonts\msyh.ttc"),
        format!(r"{windir}\Fonts\msyh.ttf"),
        format!(r"{windir}\Fonts\simhei.ttf"),
        format!(r"{windir}\Fonts\simsun.ttc"),
        format!(r"{windir}\Fonts\Deng.ttf"),
    ];
    for path in &candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert("cjk".into(), egui::FontData::from_owned(bytes));
            // 放在字体链最前：中西文统一渲染，杜绝缺字方框
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(list) = fonts.families.get_mut(&family) {
                    list.insert(0, "cjk".into());
                }
            }
            ctx.set_fonts(fonts);
            logger::info(&format!("已加载中文字体: {path}"));
            return;
        }
    }
    logger::warn("未找到系统中文字体，界面中文可能显示异常");
}

fn install_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.text_styles = [
        (egui::TextStyle::Body, egui::FontId::proportional(14.0)),
        (egui::TextStyle::Button, egui::FontId::proportional(14.0)),
        (egui::TextStyle::Small, egui::FontId::proportional(12.0)),
        (egui::TextStyle::Heading, egui::FontId::proportional(17.0)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    ctx.set_style(style);
}

// ===================== 应用状态 =====================

pub struct App {
    shared: Arc<Shared>,
    config: Arc<Mutex<Config>>,
    config_dir: PathBuf,
    engine: Arc<Mutex<Engine>>,
    ctx: Option<egui::Context>,

    // 扫描（后台线程 → 生成号 → GUI 拉取）
    scan_store: Arc<Mutex<(u64, Vec<SaveInfo>)>>,
    scan_running: Arc<AtomicBool>,
    scan_gen: u64,
    last_scan_at: Option<Instant>,
    scan: Vec<SaveInfo>,
    maps: Vec<(String, Vec<SaveInfo>)>,

    // 选择
    selected_key: Option<String>,
    backups: Vec<BackupEntry>,
    snapshots: Vec<BackupEntry>,
    selected_backup: Option<String>,  // 文件名（跨刷新保持）
    selected_snapshot: Option<String>,
    select_all: bool,
    lists_dirty: bool,

    // 设置编辑态
    interval_min: i64,
    versions: i64,
    daily_keep: i64,
    size_cap_mb: i64,
    compression: i64,
    save_path_text: String,
    output_text: String,
    autostart_checked: bool,

    // 恢复流程
    confirm_target: Option<BackupEntry>,
    restore_running: Arc<AtomicBool>,
    msg_popup: Option<String>,

    // 删除备份（右键菜单）
    delete_target: Option<BackupEntry>,
}

impl App {
    pub fn new(
        config: Arc<Mutex<Config>>,
        engine: Arc<Mutex<Engine>>,
        shared: Arc<Shared>,
        config_dir: PathBuf,
    ) -> Self {
        let cfg = config.lock().unwrap().clone();
        Self {
            shared,
            scan_store: Arc::new(Mutex::new((0, Vec::new()))),
            scan_running: Arc::new(AtomicBool::new(false)),
            restore_running: Arc::new(AtomicBool::new(false)),
            interval_min: (cfg.backup_interval_seconds / 60).max(1) as i64,
            versions: cfg.max_backup_versions as i64,
            daily_keep: cfg.daily_keep_days as i64,
            size_cap_mb: cfg.max_total_size_mb as i64,
            compression: cfg.compression_level as i64,
            save_path_text: cfg.save_path().to_string_lossy().to_string(),
            output_text: cfg.backup_output_path.clone(),
            autostart_checked: startup::is_enabled(),
            config,
            config_dir,
            engine,
            ctx: None,
            scan_gen: 0,
            last_scan_at: None,
            scan: Vec::new(),
            maps: Vec::new(),
            selected_key: None,
            backups: Vec::new(),
            snapshots: Vec::new(),
            selected_backup: None,
            selected_snapshot: None,
            select_all: false,
            lists_dirty: false,
            confirm_target: None,
            msg_popup: None,
            delete_target: None,
        }
    }

    // ---------- 后台动作 ----------

    fn start_scan(&mut self) {
        self.scan_running.store(true, Ordering::Relaxed);
        let store = self.scan_store.clone();
        let running = self.scan_running.clone();
        let engine = self.engine.clone();
        let cfg_arc = self.config.clone();
        std::thread::spawn(move || {
            let mut infos = engine.lock().unwrap().get_all_saves_info();
            // 首次运行自动全选
            let init_needed = {
                let c = cfg_arc.lock().unwrap();
                !c.monitored_initialized && !infos.is_empty()
            };
            if init_needed {
                let mut c = cfg_arc.lock().unwrap();
                for i in &infos {
                    c.set_monitored(&i.key, true);
                }
                c.monitored_initialized = true;
                engine.lock().unwrap().save_config(&c);
                for i in &mut infos {
                    i.monitored = true;
                }
            }
            {
                let mut s = store.lock().unwrap();
                s.0 += 1;
                s.1 = infos;
            }
            running.store(false, Ordering::Relaxed);
        });
    }

    fn trigger_backup_now(&mut self) {
        scheduler::trigger_manual(&self.engine, &self.shared);
    }

    fn set_monitored(&mut self, key: &str, val: bool) {
        let Ok(eng) = self.engine.try_lock() else {
            self.shared.set_status("备份进行中，请稍后再试");
            return;
        };
        let mut cfg = self.config.lock().unwrap();
        cfg.set_monitored(key, val);
        eng.save_config(&cfg);
        drop(cfg);
        for (_, saves) in &mut self.maps {
            for s in saves {
                if s.key == key {
                    s.monitored = val;
                }
            }
        }
    }

    fn set_all_monitored(&mut self, val: bool) {
        let Ok(eng) = self.engine.try_lock() else {
            self.shared.set_status("备份进行中，请稍后再试");
            return;
        };
        let mut cfg = self.config.lock().unwrap();
        let keys: Vec<String> = self.scan.iter().map(|s| s.key.clone()).collect();
        for k in &keys {
            cfg.set_monitored(k, val);
        }
        eng.save_config(&cfg);
        drop(cfg);
        for (_, saves) in &mut self.maps {
            for s in saves {
                s.monitored = val;
            }
        }
        self.shared
            .set_status(if val { "已全选存档" } else { "已取消全选" });
    }

    fn apply_numeric_settings(&mut self) {
        let eng = self.engine.lock().unwrap();
        let mut cfg = self.config.lock().unwrap();
        cfg.backup_interval_seconds = (self.interval_min.max(1).min(10080) as u64) * 60;
        cfg.max_backup_versions = self.versions.max(1).min(100) as u32;
        cfg.daily_keep_days = self.daily_keep.max(0).min(365) as u32;
        cfg.max_total_size_mb = self.size_cap_mb.max(0) as u64;
        cfg.compression_level = self.compression.max(1).min(9) as u32;
        eng.save_config(&cfg);
    }

    fn apply_save_path(&mut self) {
        let text = self.save_path_text.trim().to_string();
        let mut cfg = self.config.lock().unwrap();
        if text.is_empty() {
            cfg.save_path = String::new();
        } else if PathBuf::from(&text).is_dir() {
            if cfg.save_path != text {
                cfg.save_path = text.clone();
            }
        } else {
            self.save_path_text = cfg.save_path().to_string_lossy().to_string();
            self.shared.set_status("存档路径无效，已还原");
            return;
        }
        drop(cfg);
        self.engine
            .lock()
            .unwrap()
            .save_config(&self.config.lock().unwrap());
        self.shared.dirty_scan.store(true, Ordering::Relaxed);
        self.selected_key = None;
        self.lists_dirty = true;
    }

    fn apply_output_path(&mut self) {
        let text = self.output_text.trim().to_string();
        let mut cfg = self.config.lock().unwrap();
        if text.is_empty() || PathBuf::from(&text).is_dir() {
            if cfg.backup_output_path != text {
                cfg.backup_output_path = text;
            }
        } else {
            self.output_text = cfg.backup_output_path.clone();
            self.shared.set_status("备份保存位置无效，已还原");
            return;
        }
        drop(cfg);
        self.engine
            .lock()
            .unwrap()
            .save_config(&self.config.lock().unwrap());
        self.lists_dirty = true;
    }

    fn reload_lists(&mut self) {
        if !self.lists_dirty {
            return;
        }
        let Ok(eng) = self.engine.try_lock() else { return }; // 备份中：下帧重试
        let cfg = self.config.lock().unwrap().clone();
        if let Some(info) = self
            .scan
            .iter()
            .find(|s| Some(&s.key) == self.selected_key.as_ref())
            .cloned()
        {
            self.backups = eng.get_save_backups(&cfg, &info.map_name, &info.save_name, &info.save_dir);
            self.snapshots = eng.get_snapshots(&cfg, &info.map_name, &info.save_name, &info.save_dir);
        } else {
            self.backups.clear();
            self.snapshots.clear();
        }
        self.lists_dirty = false;
    }

    fn on_restore_clicked(&mut self) {
        if self.restore_running.load(Ordering::Relaxed) {
            self.shared.set_status("恢复正在进行中...");
            return;
        }
        let target = self
            .selected_backup
            .as_ref()
            .and_then(|name| self.backups.iter().find(|b| &b.filename == name).cloned())
            .or_else(|| {
                self.selected_snapshot
                    .as_ref()
                    .and_then(|name| self.snapshots.iter().find(|b| &b.filename == name).cloned())
            });
        let Some(entry) = target else {
            self.shared.set_status("请先在列表中选择一个备份");
            return;
        };
        match self.engine.try_lock() {
            Ok(eng) => {
                if eng.game_running() {
                    self.msg_popup =
                        Some("检测到《七日杀》正在运行，请先退出游戏再恢复存档。".into());
                    return;
                }
            }
            Err(_) => {
                self.shared.set_status("备份进行中，请稍候再恢复");
                return;
            }
        }
        self.confirm_target = Some(entry);
    }

    fn start_restore(&mut self, entry: BackupEntry) {
        let Some(info) = self
            .scan
            .iter()
            .find(|s| Some(&s.key) == self.selected_key.as_ref())
            .cloned()
        else {
            return;
        };
        self.restore_running.store(true, Ordering::Relaxed);
        let engine = self.engine.clone();
        let shared = self.shared.clone();
        let running = self.restore_running.clone();
        std::thread::spawn(move || {
            shared.set_status("正在恢复存档...");
            let result = engine.lock().unwrap().restore_backup(
                &entry.path,
                &info.map_name,
                &info.save_name,
                &info.save_dir,
            );
            match result {
                Ok(()) => {
                    shared.set_status("恢复完成");
                    shared.dirty_scan.store(true, Ordering::Relaxed);
                }
                Err(e) => {
                    let m = e.to_string();
                    logger::error(&m);
                    shared.set_status(m.clone());
                    shared.notify("恢复失败", &m);
                }
            }
            running.store(false, Ordering::Relaxed);
        });
    }

    fn start_delete(&mut self, entry: BackupEntry) {
        match self.engine.lock().unwrap().delete_backup_file(&entry.path) {
            Ok(()) => {
                self.shared
                    .set_status(format!("已删除备份: {}", entry.filename));
                if self.selected_backup.as_deref() == Some(entry.filename.as_str()) {
                    self.selected_backup = None;
                }
                if self.selected_snapshot.as_deref() == Some(entry.filename.as_str()) {
                    self.selected_snapshot = None;
                }
                self.lists_dirty = true;
            }
            Err(e) => {
                self.shared.set_status(e.clone());
                self.shared.notify("删除失败", &e);
            }
        }
    }

    /// 备份行右键菜单：显示存放位置 / 打开文件夹 / 复制路径 / 删除
    fn draw_entry_menu(
        &self,
        resp: &egui::Response,
        b: &BackupEntry,
        open_path: &mut Option<PathBuf>,
        copy_path: &mut Option<String>,
        delete_me: &mut Option<BackupEntry>,
    ) {
        resp.context_menu(|ui| {
            ui.label(egui::RichText::new("存放位置").strong());
            ui.label(egui::RichText::new(b.path.to_string_lossy()).small().weak());
            ui.separator();
            if ui.button("打开所在文件夹").clicked() {
                *open_path = Some(b.path.clone());
                ui.close_menu();
            }
            if ui.button("复制路径").clicked() {
                *copy_path = Some(b.path.to_string_lossy().to_string());
                ui.close_menu();
            }
            ui.separator();
            if ui
                .button(egui::RichText::new("删除此备份").color(DANGER))
                .clicked()
            {
                *delete_me = Some(b.clone());
                ui.close_menu();
            }
        });
    }

    fn draw_delete_window(&mut self, ctx: &egui::Context) {
        let Some(entry) = self.delete_target.clone() else { return };
        let mut open = true;
        egui::Window::new("删除备份")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("确定要删除这个备份文件吗？")
                        .strong()
                        .color(DANGER),
                );
                ui.label("删除的是备份压缩包本身，不影响当前存档；此操作无法撤销。");
                ui.separator();
                egui::Grid::new("delete_grid")
                    .num_columns(2)
                    .spacing([10.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("文件:");
                        ui.label(&entry.filename);
                        ui.end_row();
                        ui.label("时间:");
                        ui.label(&entry.time);
                        ui.end_row();
                        ui.label("大小:");
                        ui.label(format!("{:.1} MB", entry.size_mb));
                        ui.end_row();
                        ui.label("位置:");
                        ui.label(egui::RichText::new(entry.path.to_string_lossy()).small());
                        ui.end_row();
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .button(egui::RichText::new("确认删除").color(DANGER))
                        .clicked()
                    {
                        self.start_delete(entry.clone());
                        self.delete_target = None;
                    }
                    if ui.button("取消").clicked() {
                        self.delete_target = None;
                    }
                });
            });
        if !open {
            self.delete_target = None;
        }
    }

    fn selected_info(&self) -> Option<&SaveInfo> {
        self.scan
            .iter()
            .find(|s| Some(&s.key) == self.selected_key.as_ref())
    }

    fn busy_now(&self) -> bool {
        let s = self.shared.status_text();
        s.contains("备份中") || s.contains("恢复中") || self.restore_running.load(Ordering::Relaxed)
    }

    fn failed_last(&self) -> bool {
        let s = self.shared.status_text();
        s.contains("失败") || s.contains("错误")
    }

    // ---------- 界面 ----------

    /// 顶部状态栏：状态点 + 状态 + 下次备份 + 操作按钮
    fn draw_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let (dot, dot_color) = if self.busy_now() {
                    ("●", egui::Color32::from_rgb(245, 160, 0))
                } else if self.failed_last() {
                    ("●", DANGER)
                } else {
                    ("●", ACCENT)
                };
                ui.label(egui::RichText::new(dot).color(dot_color).size(12.0));
                ui.label(egui::RichText::new(self.shared.status_text()).strong());
                ui.separator();
                ui.label(format!("下次备份: {}", self.shared.next_backup_text()));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("隐藏窗口").clicked() {
                        if let Some(ctx) = &self.ctx {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                    }
                    if ui.button("立即备份").clicked() {
                        self.trigger_backup_now();
                    }
                });
            });
            ui.add_space(6.0);
            ui.separator();
        });
    }

    /// 左侧存档列表 + 右侧备份历史
    fn draw_main(&mut self, ctx: &egui::Context) {
        let maps = self.maps.clone();
        let maps_empty = maps.is_empty();

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::SidePanel::left("saves_panel")
                .resizable(true)
                .default_width(400.0)
                .width_range(260.0..=640.0)
                .show_inside(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("存档列表").heading());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.checkbox(&mut self.select_all, "全选").changed() {
                                let val = self.select_all;
                                self.set_all_monitored(val);
                            }
                        });
                    });
                    ui.separator();

                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        if maps_empty {
                            ui.add_space(12.0);
                            ui.label(
                                egui::RichText::new("未找到存档，请检查下方「存档路径」").weak(),
                            );
                        }
                        for (map_name, saves) in &maps {
                            egui::CollapsingHeader::new(
                                egui::RichText::new(map_name.clone()).strong(),
                            )
                            .default_open(true)
                            .show(ui, |ui| {
                                for info in saves {
                                    self.draw_save_row(ui, info);
                                }
                            });
                        }
                    });
                });

            egui::CentralPanel::default().show_inside(ui, |ui| {
                self.draw_backup_lists(ui);
            });
        });
    }

    fn draw_save_row(&mut self, ui: &mut egui::Ui, info: &SaveInfo) {
        let row = ui.horizontal(|ui| {
            let mut checked = info.monitored;
            if ui.checkbox(&mut checked, "").changed() {
                self.set_monitored(&info.key, checked);
            }
            let selected = self.selected_key.as_deref() == Some(info.key.as_str());
            if ui
                .selectable_label(selected, egui::RichText::new(&info.save_name).strong())
                .clicked()
            {
                if self.selected_key.as_deref() != Some(info.key.as_str()) {
                    self.selected_key = Some(info.key.clone());
                    self.selected_backup = None;
                    self.selected_snapshot = None;
                    self.lists_dirty = true;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // "2026-10-04 14:20:42" -> "10-04 14:20"；从未备份等原文显示
                let short = if info.last_backup.starts_with("2") && info.last_backup.len() >= 16 {
                    info.last_backup[5..16].to_string()
                } else {
                    info.last_backup.clone()
                };
                ui.label(egui::RichText::new(short).small().weak());
            });
        });
        // 点击行内空白处也能选中该存档
        if row.response.clicked() && self.selected_key.as_deref() != Some(info.key.as_str()) {
            self.selected_key = Some(info.key.clone());
            self.selected_backup = None;
            self.selected_snapshot = None;
            self.lists_dirty = true;
        }
        row.response
            .on_hover_text(format!("上次备份: {}\n最后修改: {}", info.last_backup, info.last_modified));
        ui.separator();
    }

    fn draw_backup_lists(&mut self, ui: &mut egui::Ui) {
        let title = self
            .selected_info()
            .map(|i| format!("备份历史 — {}", i.key))
            .unwrap_or_else(|| "备份历史".to_string());
        ui.label(egui::RichText::new(title).heading());
        ui.separator();

        if self.selected_info().is_none() {
            ui.add_space(12.0);
            ui.label(egui::RichText::new("在左侧点选一个存档，查看它的备份记录").weak());
            return;
        }

        let mut open_path: Option<PathBuf> = None;
        let mut copy_path: Option<String> = None;
        let mut delete_me: Option<BackupEntry> = None;

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.label(egui::RichText::new("备份记录（右键可管理）").strong());
            if self.backups.is_empty() {
                ui.label(egui::RichText::new("（暂无备份）").weak());
            }
            for b in &self.backups {
                let sel = self.selected_backup.as_deref() == Some(b.filename.as_str());
                let text = format!("{}   {:.1} MB", b.time, b.size_mb);
                let resp = ui.selectable_label(sel, text);
                if resp.clicked() {
                    self.selected_backup = Some(b.filename.clone());
                    self.selected_snapshot = None;
                }
                self.draw_entry_menu(&resp, b, &mut open_path, &mut copy_path, &mut delete_me);
            }

            ui.add_space(10.0);
            ui.separator();
            ui.label(egui::RichText::new("恢复前快照").strong().color(ACCENT));
            if self.snapshots.is_empty() {
                ui.label(
                    egui::RichText::new("（暂无快照，恢复存档时会自动创建）").weak(),
                );
            }
            for b in &self.snapshots {
                let sel = self.selected_snapshot.as_deref() == Some(b.filename.as_str());
                let text = format!("{}   {:.1} MB", b.time, b.size_mb);
                let resp = ui.selectable_label(sel, text);
                if resp.clicked() {
                    self.selected_snapshot = Some(b.filename.clone());
                    self.selected_backup = None;
                }
                self.draw_entry_menu(&resp, b, &mut open_path, &mut copy_path, &mut delete_me);
            }
        });

        // 应用右键动作
        if let Some(p) = open_path {
            Engine::reveal_in_explorer(&p);
        }
        if let Some(p) = copy_path {
            ui.ctx().output_mut(|o| o.copied_text = p);
            self.shared.set_status("备份路径已复制到剪贴板");
        }
        if let Some(d) = delete_me {
            self.delete_target = Some(d);
        }

        ui.add_space(8.0);
        let busy = self.restore_running.load(Ordering::Relaxed);
        let label = if busy {
            egui::RichText::new("恢复中...")
        } else {
            egui::RichText::new("恢复选中的备份").strong()
        };
        let btn = ui.add_sized([ui.available_width(), 34.0], egui::Button::new(label));
        if btn.clicked() {
            self.on_restore_clicked();
        }
    }

    /// 底部设置：折叠分组，保持主界面简洁
    fn draw_settings(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("settings").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.separator();

            egui::CollapsingHeader::new(egui::RichText::new("备份设置").strong())
                .default_open(true)
                .show(ui, |ui| {
                    egui::Grid::new("settings_grid")
                        .num_columns(4)
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("备份间隔");
                            if ui
                                .add(
                                    egui::DragValue::new(&mut self.interval_min)
                                        .range(1..=10080)
                                        .speed(1)
                                        .suffix(" 分钟"),
                                )
                                .changed()
                            {
                                self.apply_numeric_settings();
                            }
                            ui.label("保留版本");
                            if ui
                                .add(
                                    egui::DragValue::new(&mut self.versions)
                                        .range(1..=100)
                                        .speed(1)
                                        .suffix(" 个"),
                                )
                                .changed()
                            {
                                self.apply_numeric_settings();
                            }
                            ui.end_row();

                            ui.label("每日额外保留");
                            if ui
                                .add(
                                    egui::DragValue::new(&mut self.daily_keep)
                                        .range(0..=365)
                                        .speed(1)
                                        .suffix(" 天"),
                                )
                                .changed()
                            {
                                self.apply_numeric_settings();
                            }
                            ui.label("总量上限");
                            if ui
                                .add(
                                    egui::DragValue::new(&mut self.size_cap_mb)
                                        .range(0..=10_000_000)
                                        .speed(64)
                                        .suffix(" MB"),
                                )
                                .changed()
                            {
                                self.apply_numeric_settings();
                            }
                            ui.end_row();

                            ui.label("压缩级别");
                            if ui
                                .add(
                                    egui::DragValue::new(&mut self.compression)
                                        .range(1..=9)
                                        .speed(1),
                                )
                                .changed()
                            {
                                self.apply_numeric_settings();
                            }
                            ui.label(egui::RichText::new("总量 0 MB = 不限量").small().weak());
                            ui.end_row();
                        });
                    ui.checkbox(&mut self.autostart_checked, "开机自启");
                });

            egui::CollapsingHeader::new(egui::RichText::new("路径设置").strong())
                .default_open(true)
                .show(ui, |ui| {
                    // 存档路径
                    ui.horizontal(|ui| {
                        ui.add_sized([92.0, 20.0], egui::Label::new("存档路径"));
                        let w = (ui.available_width() - 92.0).max(160.0);
                        let resp = egui::TextEdit::singleline(&mut self.save_path_text)
                            .desired_width(w)
                            .show(ui);
                        if resp.response.lost_focus() {
                            self.apply_save_path();
                        }
                        if ui.button("浏览...").clicked() {
                            let current = PathBuf::from(&self.save_path_text);
                            let dir = if current.is_dir() {
                                current
                            } else {
                                self.config.lock().unwrap().save_path()
                            };
                            if let Some(picked) = rfd::FileDialog::new()
                                .set_title("选择七日杀存档目录")
                                .set_directory(&dir)
                                .pick_folder()
                            {
                                if picked != dir {
                                    self.save_path_text = picked.to_string_lossy().to_string();
                                    self.apply_save_path();
                                }
                            }
                        }
                    });
                    // 备份保存位置
                    ui.horizontal(|ui| {
                        ui.add_sized([92.0, 20.0], egui::Label::new("备份保存位置"));
                        let w = (ui.available_width() - 92.0).max(160.0);
                        let resp = egui::TextEdit::singleline(&mut self.output_text)
                            .desired_width(w)
                            .show(ui);
                        if resp.response.lost_focus() {
                            self.apply_output_path();
                        }
                        if ui.button("浏览...").clicked() {
                            let current = if self.output_text.trim().is_empty() {
                                self.config.lock().unwrap().save_path()
                            } else {
                                PathBuf::from(self.output_text.trim())
                            };
                            let dir = if current.is_dir() {
                                current
                            } else {
                                self.config.lock().unwrap().save_path()
                            };
                            if let Some(picked) = rfd::FileDialog::new()
                                .set_title("选择备份保存位置")
                                .set_directory(&dir)
                                .pick_folder()
                            {
                                if Some(&picked) != Some(&dir) {
                                    self.output_text = picked.to_string_lossy().to_string();
                                    self.apply_output_path();
                                }
                            }
                        }
                    });
                    ui.label(
                        egui::RichText::new(
                            "备份保存位置留空 = 存到每个存档目录内的 backup 文件夹",
                        )
                        .small()
                        .weak(),
                    );
                });

            ui.label(
                egui::RichText::new(format!(
                    "v{} · Developed by Lumore · Rust 原生版",
                    env!("CARGO_PKG_VERSION")
                ))
                .small()
                .weak(),
            );
            ui.add_space(4.0);
        });
    }

    fn draw_confirm_window(&mut self, ctx: &egui::Context) {
        let Some(entry) = self.confirm_target.clone() else { return };
        let mut open = true;
        egui::Window::new("确认恢复")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("恢复会覆盖当前存档！")
                        .strong()
                        .color(DANGER),
                );
                ui.separator();
                egui::Grid::new("confirm_grid")
                    .num_columns(2)
                    .spacing([10.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("备份文件:");
                        ui.label(&entry.filename);
                        ui.end_row();
                        ui.label("备份时间:");
                        ui.label(&entry.time);
                        ui.end_row();
                        ui.label("大小:");
                        ui.label(format!("{:.1} MB", entry.size_mb));
                        ui.end_row();
                    });
                ui.label("恢复前会自动创建安全快照，若出问题可从快照再恢复。");
                ui.separator();
                ui.horizontal(|ui| {
                    let busy = self.restore_running.load(Ordering::Relaxed);
                    if ui
                        .add_enabled(
                            !busy,
                            egui::Button::new(egui::RichText::new("确认恢复").strong()),
                        )
                        .clicked()
                    {
                        self.start_restore(entry.clone());
                        self.confirm_target = None;
                    }
                    if ui.button("取消").clicked() {
                        self.confirm_target = None;
                    }
                });
            });
        if !open {
            self.confirm_target = None;
        }
    }

    fn draw_msg_popup(&mut self, ctx: &egui::Context) {
        let Some(msg) = self.msg_popup.clone() else { return };
        let mut open = true;
        egui::Window::new("提示")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(msg);
                ui.separator();
                if ui.button("确定").clicked() {
                    self.msg_popup = None;
                }
            });
        if !open {
            self.msg_popup = None;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ctx = Some(ctx.clone());

        // ---- 托盘菜单事件由托盘线程直接处理（窗口隐藏时 GUI 循环停摆，不能依赖这里）----
        // 此处仅消费"显示设置"请求：托盘线程已用 ShowWindow 恢复窗口，这里负责聚焦。

        // ---- 标志处理 ----
        if self.shared.show_requested.swap(false, Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if self.shared.backup_now.swap(false, Ordering::Relaxed) {
            self.trigger_backup_now();
        }
        if self.shared.open_backup_folder.swap(false, Ordering::Relaxed) {
            let cfg = self.config.lock().unwrap().clone();
            std::thread::spawn(move || {
                let custom = cfg.backup_output_path.trim().to_string();
                let dir = if !custom.is_empty() && PathBuf::from(&custom).is_dir() {
                    PathBuf::from(custom)
                } else {
                    cfg.save_path()
                };
                if dir.is_dir() {
                    let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                }
            });
        }
        if self.shared.is_exit() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // ---- 周期扫描 ----
        if !self.scan_running.load(Ordering::Relaxed) {
            let due = self
                .last_scan_at
                .map(|t| t.elapsed() >= SCAN_INTERVAL)
                .unwrap_or(true)
                || self.shared.dirty_scan.load(Ordering::Relaxed);
            if due {
                self.shared.dirty_scan.store(false, Ordering::Relaxed);
                self.last_scan_at = Some(Instant::now());
                self.start_scan();
            }
        }

        // ---- 拉取扫描结果 ----
        let new_gen = {
            let s = self.scan_store.lock().unwrap();
            if s.0 != self.scan_gen {
                Some((s.0, s.1.clone()))
            } else {
                None
            }
        };
        if let Some((gen, infos)) = new_gen {
            self.scan_gen = gen;
            self.scan = infos;
            let mut grouped: BTreeMap<String, Vec<SaveInfo>> = BTreeMap::new();
            for info in &self.scan {
                grouped.entry(info.map_name.clone()).or_default().push(info.clone());
            }
            self.maps = grouped.into_iter().collect();
            self.lists_dirty = true;
            // 从未选中过存档时，自动选中最近备份过的那个（否则右侧永远提示"先点选"）
            if self.selected_key.is_none() {
                let candidate = self
                    .scan
                    .iter()
                    .filter(|s| s.last_backup.starts_with("2"))
                    .max_by(|a, b| a.last_backup.cmp(&b.last_backup))
                    .or_else(|| self.scan.first())
                    .map(|s| s.key.clone());
                if let Some(key) = candidate {
                    self.selected_key = Some(key);
                }
            }
        }
        self.reload_lists();

        // ---- 窗口关闭 → 最小化到托盘（退出时放行）----
        // 注意：不能用 Visible(false) —— eframe 0.29/winit 0.30 组合下隐藏会连带销毁主窗口；
        // 用 Minimized 保持窗口存活（egui 循环继续运行，托盘"显示设置"也能可靠还原）
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested {
            let exit_requested = self.shared.is_exit();
            let minimize = self.config.lock().unwrap().minimize_to_tray;
            if !exit_requested && minimize {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
        }

        // ---- 绘制 ----
        self.draw_top_bar(ctx);
        self.draw_main(ctx);
        self.draw_settings(ctx);
        self.draw_confirm_window(ctx);
        self.draw_delete_window(ctx);
        self.draw_msg_popup(ctx);

        ctx.request_repaint_after(Duration::from_millis(400));
    }
}

// ---------- 入口 ----------

fn load_icon_data() -> Option<egui::IconData> {
    const PNG: &[u8] = include_bytes!("../../resources/icon.png");
    let img = image::load_from_memory(PNG).ok()?;
    let img = img.to_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData { width, height, rgba: img.into_raw() })
}

pub fn run(
    config: Arc<Mutex<Config>>,
    engine: Arc<Mutex<Engine>>,
    shared: Arc<Shared>,
    config_dir: PathBuf,
    hidden: bool,
) -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("7 Days to Die 存档备份工具")
        .with_inner_size([900.0, 660.0])
        .with_min_inner_size([760.0, 540.0]);
    if hidden {
        viewport = viewport.with_visible(false);
    }
    if let Some(icon) = load_icon_data() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "7DaysBackup",
        options,
        Box::new(move |cc| {
            install_fonts(&cc.egui_ctx);
            install_style(&cc.egui_ctx);
            Ok(Box::new(App::new(config, engine, shared, config_dir)))
        }),
    )
}
