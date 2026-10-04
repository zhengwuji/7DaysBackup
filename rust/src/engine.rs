//! 备份引擎
//! - 扫描存档、清单变更检测（mtime+size 精确对比，不受时钟调整影响）
//! - 备份：临时文件写在输出目录内（同卷原子改名，支持输出到另一块盘）、
//!         输出盘空间检查、压缩级别可调、被占用文件计数、写后完整性校验
//! - 分层清理：最近 N 个 + 尾部每天最新 1 个额外保留 N 天 + 可选总量上限
//! - 恢复：zip 路径校验（防穿越）、游戏进程检测、恢复前快照、临时目录中转

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};
use serde::Serialize;
use walkdir::WalkDir;

use crate::app_shared::Shared;
use crate::config::Config;
use crate::logger;

const SNAPSHOT_DIR: &str = "snapshots";
const MAX_SNAPSHOTS: usize = 3;

// ===================== 数据类型 =====================

#[derive(Clone, Debug)]
pub struct SaveInfo {
    pub map_name: String,
    pub save_name: String,
    pub save_dir: PathBuf,
    pub key: String,
    pub monitored: bool,
    pub last_backup: String,
    pub last_modified: String,
}

#[derive(Clone, Debug)]
pub struct BackupEntry {
    pub filename: String,
    pub path: PathBuf,
    pub size_mb: f64,
    pub time: String,
}

#[derive(Serialize, Clone)]
pub struct BackupStatus {
    pub total: usize,
    pub backed_up: usize,
    pub failed: usize,
    pub cleaned: usize,
    pub skipped: usize,
    pub message: String,
}

#[derive(Debug)]
pub enum RestoreError {
    GameRunning,
    InvalidZip(String),
    Snapshot(String),
    Io(String),
}

impl std::fmt::Display for RestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestoreError::GameRunning => write!(f, "检测到《七日杀》正在运行，请先退出游戏再恢复存档"),
            RestoreError::InvalidZip(s) => write!(f, "备份文件包含非法路径，已中止恢复: {s}"),
            RestoreError::Snapshot(s) => write!(f, "恢复前快照创建失败: {s}"),
            RestoreError::Io(s) => write!(f, "恢复失败: {s}"),
        }
    }
}

struct CollectedFile {
    path: PathBuf,
    rel: String, // 正斜杠相对路径
    mtime_nanos: u64,
    size: u64,
}

pub struct Engine {
    config: Arc<Mutex<Config>>,
    config_dir: PathBuf,
    shared: Arc<Shared>,
}

// ===================== 基础 =====================

impl Engine {
    pub fn new(config: Arc<Mutex<Config>>, config_dir: PathBuf, shared: Arc<Shared>) -> Self {
        Self { config, config_dir, shared }
    }

    pub fn save_config(&self, cfg: &Config) {
        cfg.save(&self.config_dir);
    }

    fn fmt_time(st: std::time::SystemTime) -> String {
        let dt: DateTime<Local> = st.into();
        dt.format("%Y-%m-%d %H:%M:%S").to_string()
    }

    fn now_stamp() -> String {
        Local::now().format("%Y%m%d_%H%M%S%3f").to_string()
    }

    fn now_display() -> String {
        Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
    }

    fn mtime_nanos(st: std::time::SystemTime) -> u64 {
        st.duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }

    /// path 是否等于 root 或位于 root 之下（Windows 忽略大小写）
    fn is_under(path: &Path, root: &Path) -> bool {
        let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
        let (p, r) = (norm(path), norm(root));
        p == r || p.starts_with(&format!("{r}\\"))
    }

    /// 校验 zip 条目名，拒绝绝对路径与 .. 上跳（防路径穿越）
    fn safe_zip_name(name: &str) -> bool {
        let n = name.replace('\\', "/");
        if n.is_empty() || n.starts_with('/') || n.contains(':') {
            return false;
        }
        !n.split('/').any(|seg| seg == "..")
    }

    /// 检测《七日杀》是否正在运行
    pub fn game_running(&self) -> bool {
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        sys.processes().values().any(|p| {
            p.name()
                .to_string_lossy()
                .to_lowercase()
                .contains("7daystodie")
        })
    }

    /// 删除单个备份/快照文件
    pub fn delete_backup_file(&self, path: &Path) -> Result<(), String> {
        fs::remove_file(path).map_err(|e| format!("删除失败: {e}"))?;
        logger::info(&format!("已删除备份文件: {}", path.display()));
        Ok(())
    }

    /// 在资源管理器中打开文件夹并选中该文件
    pub fn reveal_in_explorer(path: &Path) {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn();
    }
}

// ===================== 扫描 / 清单 =====================

impl Engine {
    pub fn scan_saves(&self, cfg: &Config) -> Vec<(String, String, PathBuf, String)> {
        let mut saves = Vec::new();
        let root = cfg.save_path();
        let Ok(map_entries) = fs::read_dir(&root) else {
            logger::warn(&format!("存档目录不存在: {}", root.display()));
            return saves;
        };
        for map_entry in map_entries.flatten() {
            let map_name = map_entry.file_name().to_string_lossy().to_string();
            if !map_entry.path().is_dir() || map_name.starts_with('.') {
                continue;
            }
            let Ok(save_entries) = fs::read_dir(map_entry.path()) else { continue };
            for save_entry in save_entries.flatten() {
                let save_name = save_entry.file_name().to_string_lossy().to_string();
                if !save_entry.path().is_dir() || save_name.starts_with('.') {
                    continue;
                }
                saves.push((
                    map_name.clone(),
                    save_name.clone(),
                    save_entry.path(),
                    format!("{map_name}/{save_name}"),
                ));
            }
        }
        saves
    }

    /// 遍历存档目录文件（排除备份输出目录与名为 backup 的目录）
    fn collect_files(save_dir: &Path, out_dir: &Path) -> Vec<CollectedFile> {
        let mut files = Vec::new();
        for entry in WalkDir::new(save_dir)
            .into_iter()
            .filter_entry(|e| {
                if !e.file_type().is_dir() {
                    return true;
                }
                let p = e.path();
                if p == save_dir {
                    return true;
                }
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if name == "backup" {
                    return false; // 默认备份目录（含历史遗留）
                }
                !Engine::is_under(p, out_dir)
            })
            .flatten()
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let rel = entry
                .path()
                .strip_prefix(save_dir)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .replace('\\', "/");
            files.push(CollectedFile {
                path: entry.path().to_path_buf(),
                rel,
                mtime_nanos: Engine::mtime_nanos(meta.modified().unwrap_or(std::time::UNIX_EPOCH)),
                size: meta.len(),
            });
        }
        files.sort_by(|a, b| a.rel.cmp(&b.rel));
        files
    }

    fn to_manifest(files: &[CollectedFile]) -> HashMap<String, [u64; 2]> {
        files
            .iter()
            .map(|f| (f.rel.clone(), [f.mtime_nanos, f.size]))
            .collect()
    }

    /// 检查被监控存档的变化，返回发生变化的存档
    pub fn check_saves_changed(&self, cfg: &Config) -> Vec<(String, String, PathBuf, String)> {
        let mut changed = Vec::new();
        for (map, save, save_dir, key) in self.scan_saves(cfg) {
            if !cfg.is_monitored(&key) {
                continue;
            }
            let out_dir = cfg.backup_dir(&map, &save, &save_dir);
            let current = Self::to_manifest(&Self::collect_files(&save_dir, &out_dir));
            let stored = cfg.manifests.get(&key);
            if stored.is_none_or(|m| *m != current) {
                changed.push((map, save, save_dir, key));
            }
        }
        changed
    }

    /// 扫描全部存档信息（供 GUI 显示），并顺手清理失效存档的残留配置。
    /// 返回是否有配置变更（需要保存）。
    pub fn get_all_saves_info(&mut self) -> Vec<SaveInfo> {
        let mut cfg = self.config.lock().unwrap().clone();
        let scan = self.scan_saves(&cfg);
        let keys: Vec<String> = scan.iter().map(|s| s.3.clone()).collect();
        let pruned = cfg.prune_stale_entries(&cfg.save_path(), &keys);
        let mut infos = Vec::new();
        for (map, save, save_dir, key) in &scan {
            let out_dir = cfg.backup_dir(map, save, save_dir);
            let last_modified = latest_mtime(&Self::collect_files(save_dir, &out_dir))
                .map(Self::fmt_time)
                .unwrap_or_else(|| "未知".into());
            infos.push(SaveInfo {
                map_name: map.clone(),
                save_name: save.clone(),
                save_dir: save_dir.clone(),
                key: key.clone(),
                monitored: cfg.is_monitored(key),
                last_backup: cfg
                    .last_backup_times
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| "从未备份".into()),
                last_modified,
            });
        }
        if pruned {
            self.save_config(&cfg);
            *self.config.lock().unwrap() = cfg;
        }
        infos
    }
}

fn latest_mtime(files: &[CollectedFile]) -> Option<std::time::SystemTime> {
    files
        .iter()
        .map(|f| std::time::UNIX_EPOCH + std::time::Duration::from_nanos(f.mtime_nanos))
        .max()
}

// ===================== 备份 =====================

impl Engine {
    /// 对单个存档备份。临时 zip 写在输出目录内（与最终文件同卷，rename 原子），
    /// 写入后做完整性校验，被占用跳过的文件不计入清单（下一轮自动重试）。
    pub fn backup_save(
        &mut self,
        map: &str,
        save: &str,
        save_dir: &Path,
        key: &str,
        snapshot: bool,
    ) -> Result<usize, String> {
        let cfg = self.config.lock().unwrap().clone();
        let out_dir = cfg.backup_dir(map, save, save_dir);
        let out_dir = if snapshot {
            out_dir.join(SNAPSHOT_DIR)
        } else {
            out_dir
        };
        fs::create_dir_all(&out_dir).map_err(|e| format!("创建备份目录失败: {e}"))?;

        let files = Self::collect_files(save_dir, &out_dir);
        let total_size: u64 = files.iter().map(|f| f.size).sum();

        let filename = format!("{map}_{save}_{}.zip", Self::now_stamp());
        let tmp_path = out_dir.join(format!("_{filename}.tmp"));
        let final_path = out_dir.join(&filename);
        logger::info(&format!("开始备份: {map}/{save} -> {}", final_path.display()));

        // 输出目录所在盘的空间检查（源大小 × 1.2 + 10MB 余量）
        match fs2::available_space(&out_dir) {
            Ok(free) if free < total_size.saturating_mul(12) / 10 + 10 * 1024 * 1024 => {
                let msg = format!(
                    "磁盘空间不足（需要约 {:.1} MB，剩余 {:.1} MB），跳过备份 {map}/{save}",
                    total_size as f64 * 1.2 / 1048576.0,
                    free as f64 / 1048576.0
                );
                logger::error(&msg);
                return Err(msg);
            }
            _ => {} // 无法获取剩余空间时不阻塞备份
        }

        let level = cfg.compression_level.clamp(1, 9) as i64;
        let mut skipped = 0usize;
        let mut kept: Vec<CollectedFile> = Vec::new();

        let write_result = (|| -> io::Result<()> {
            let file = fs::File::create(&tmp_path)?;
            let mut zw = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(level));
            for f in &files {
                match fs::File::open(&f.path) {
                    Err(e) => {
                        skipped += 1;
                        logger::warn(&format!("跳过文件 {}: {}（可能被游戏占用）", f.path.display(), e));
                    }
                    Ok(mut fh) => match zw.start_file(f.rel.clone(), options) {
                        Err(e) => {
                            skipped += 1;
                            logger::warn(&format!("跳过文件 {}: {e}", f.path.display()));
                        }
                        Ok(()) => match io::copy(&mut fh, &mut zw) {
                            Ok(_) => kept.push(CollectedFile {
                                path: f.path.clone(),
                                rel: f.rel.clone(),
                                mtime_nanos: f.mtime_nanos,
                                size: f.size,
                            }),
                            Err(e) => {
                                // 中断半写入的条目，保持归档其余部分合法
                                let _ = zw.abort_file();
                                skipped += 1;
                                logger::warn(&format!(
                                    "跳过文件 {}: {e}（可能被游戏占用）",
                                    f.path.display()
                                ));
                            }
                        },
                    }
                }
            }
            zw.finish()?;
            Ok(())
        })();

        if let Err(e) = write_result {
            let _ = fs::remove_file(&tmp_path);
            let msg = format!("备份 {map}/{save} 失败: {e}");
            logger::error(&msg);
            return Err(msg);
        }

        // 原子改名（同卷）
        if final_path.exists() {
            let _ = fs::remove_file(&final_path);
        }
        if let Err(e) = fs::rename(&tmp_path, &final_path) {
            let _ = fs::remove_file(&tmp_path);
            let msg = format!("备份 {map}/{save} 落盘失败: {e}");
            logger::error(&msg);
            return Err(msg);
        }

        // 完整性校验：逐条目解码并验证 CRC，杜绝半成品备份
        if let Err(e) = verify_zip(&final_path) {
            let _ = fs::remove_file(&final_path);
            let msg = format!("备份 {map}/{save} 完整性校验失败，已删除坏档: {e}");
            logger::error(&msg);
            return Err(msg);
        }

        let zip_size = fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
        logger::info(&format!(
            "备份完成: {filename} ({} 个文件, 原始 {:.1} MB, 压缩后 {:.1} MB{})",
            kept.len(),
            total_size as f64 / 1048576.0,
            zip_size as f64 / 1048576.0,
            if skipped > 0 { format!(", 跳过 {skipped} 个文件") } else { String::new() }
        ));

        if snapshot {
            prune_snapshots(&out_dir);
            return Ok(skipped);
        }

        // 记录备份时间与清单（以实际写入 zip 的内容为准）
        let mut cfg = self.config.lock().unwrap();
        cfg.last_backup_times
            .insert(key.to_string(), Self::now_display());
        cfg.manifests.insert(key.to_string(), Self::to_manifest(&kept));
        self.save_config(&cfg);
        Ok(skipped)
    }

    /// 完整备份周期：检测变化 → 逐个备份 → 全部存档分层清理
    pub fn run_backup(&mut self) -> BackupStatus {
        let mut status = BackupStatus {
            total: 0,
            backed_up: 0,
            failed: 0,
            cleaned: 0,
            skipped: 0,
            message: String::new(),
        };
        logger::info("=== 开始备份周期 ===");
        let cfg = self.config.lock().unwrap().clone();
        let changed = self.check_saves_changed(&cfg);
        status.total = changed.len();

        if changed.is_empty() {
            status.message = "没有检测到变化的存档".into();
        } else {
            for (map, save, save_dir, key) in &changed {
                match self.backup_save(map, save, save_dir, key, false) {
                    Ok(skipped) => {
                        status.backed_up += 1;
                        status.skipped += skipped;
                        // 保留策略：只有产生了新备份，才对该存档清理旧备份；
                        // 没有新备份就绝不删除任何旧备份
                        status.cleaned += self.cleanup_old_backups(map, save, save_dir);
                    }
                    Err(e) => {
                        status.failed += 1;
                        logger::error(&e);
                    }
                }
            }
            status.message = format!(
                "备份完成: 检测到 {} 个变化存档, 成功 {}, 失败 {}, 清理 {} 个旧备份{}",
                status.total,
                status.backed_up,
                status.failed,
                status.cleaned,
                if status.skipped > 0 {
                    format!("，跳过 {} 个被占用文件（下次自动重试）", status.skipped)
                } else {
                    String::new()
                }
            );
        }
        logger::info(&status.message);
        if status.failed > 0 {
            self.shared.notify("备份失败", &status.message);
        }
        status
    }

    /// 清理异常退出残留：`_*.zip.tmp` 文件与 `*_restore_tmp` 目录
    pub fn cleanup_tmp_files(&mut self) {
        let cfg = self.config.lock().unwrap().clone();
        let roots: Vec<PathBuf> = {
            let mut v = vec![cfg.save_path()];
            let custom = cfg.backup_output_path.trim();
            if !custom.is_empty() {
                v.push(PathBuf::from(custom));
            }
            v
        };
        for root in roots {
            if !root.is_dir() {
                continue;
            }
            let Ok(maps) = fs::read_dir(&root) else { continue };
            for map in maps.flatten() {
                if map.path().is_dir() {
                    remove_stale_in(&map.path());
                }
                if let Ok(saves) = fs::read_dir(map.path()) {
                    for save in saves.flatten() {
                        if save.path().is_dir() {
                            remove_stale_in(&save.path());
                        }
                    }
                }
            }
        }
    }
}

fn remove_stale_in(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if name.starts_with('_') && name.ends_with(".tmp") && path.is_file() {
            let _ = fs::remove_file(&path);
            logger::info(&format!("已清理残留临时文件: {}", path.display()));
        } else if name.ends_with("_restore_tmp") && path.is_dir() {
            let _ = fs::remove_dir_all(&path);
            logger::info(&format!("已清理残留恢复临时目录: {}", path.display()));
        }
    }
}

fn verify_zip(path: &Path) -> Result<(), String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        io::copy(&mut entry, &mut io::sink()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn prune_snapshots(snapshot_dir: &Path) {
    let mut snaps: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    if let Ok(entries) = fs::read_dir(snapshot_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "zip").unwrap_or(false) {
                if let Ok(meta) = entry.metadata() {
                    snaps.push((meta.modified().unwrap_or(std::time::UNIX_EPOCH), path));
                }
            }
        }
    }
    snaps.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in snaps.iter().skip(MAX_SNAPSHOTS) {
        let _ = fs::remove_file(path);
        logger::info(&format!("已删除旧快照: {}", path.display()));
    }
}

// ===================== 清理 =====================

impl Engine {
    /// 分层清理：保留最新 N 个；更旧的备份中每天最新 1 个额外保留 daily_keep_days 天；
    /// 可选总量上限（max_total_size_mb，超出时从最旧的保留项开始删，至少保留 1 个）
    pub fn cleanup_old_backups(&mut self, map: &str, save: &str, save_dir: &Path) -> usize {
        let cfg = self.config.lock().unwrap().clone();
        let max_versions = cfg.max_backup_versions.max(1) as usize;
        let daily_keep = cfg.daily_keep_days as usize;
        let size_cap = cfg.max_total_size_mb.saturating_mul(1024 * 1024);
        let backup_dir = cfg.backup_dir(map, save, save_dir);
        if !backup_dir.is_dir() {
            return 0;
        }

        let mut zips: Vec<(std::time::SystemTime, PathBuf, u64)> = Vec::new();
        if let Ok(entries) = fs::read_dir(&backup_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "zip").unwrap_or(false) {
                    if let Ok(meta) = entry.metadata() {
                        zips.push((meta.modified().unwrap_or(std::time::UNIX_EPOCH), path, meta.len()));
                    }
                }
            }
        }
        zips.sort_by(|a, b| b.0.cmp(&a.0)); // 最新在前
        let size_cap_active = size_cap > 0;
        if zips.len() <= max_versions && !size_cap_active {
            return 0;
        }

        // keep 列表（索引，最新在前）
        let mut keep: Vec<usize> = (0..max_versions.min(zips.len())).collect();
        if daily_keep > 0 && zips.len() > max_versions {
            // 尾部（超出最近 N 个）中，每个自然日取最新的 1 个，最多保留 daily_keep 天
            let mut seen_days = 0usize;
            let mut last_day: Option<chrono::NaiveDate> = None;
            for i in max_versions..zips.len() {
                let day: chrono::NaiveDate = DateTime::<Local>::from(zips[i].0).date_naive();
                if last_day != Some(day) {
                    last_day = Some(day);
                    seen_days += 1;
                    if seen_days > daily_keep {
                        break;
                    }
                    keep.push(i);
                }
            }
        }
        // 总量上限：超出时从最旧的保留项开始删，至少保留 1 个
        if size_cap > 0 {
            let mut total: u64 = keep.iter().map(|&i| zips[i].2).sum();
            while total > size_cap && keep.len() > 1 {
                let removed = keep.pop().unwrap(); // keep 按最新在前排列，pop = 最旧的
                total -= zips[removed].2;
            }
        }

        let keep_set: std::collections::HashSet<usize> = keep.into_iter().collect();
        let mut deleted = 0;
        for (i, (_, path, _)) in zips.iter().enumerate() {
            // 最新一份永远保留（双保险：即使上限配置异常也不删最新备份）
            if i == 0 || keep_set.contains(&i) {
                continue;
            }
            if fs::remove_file(path).is_ok() {
                logger::info(&format!("已删除旧备份: {}", path.display()));
                deleted += 1;
            } else {
                logger::warn(&format!("删除旧备份失败: {}", path.display()));
            }
        }
        deleted
    }
}

// ===================== 备份列表 / 快照 =====================

impl Engine {
    pub fn get_save_backups(&self, cfg: &Config, map: &str, save: &str, save_dir: &Path) -> Vec<BackupEntry> {
        list_zips(&cfg.backup_dir(map, save, save_dir))
    }

    pub fn get_snapshots(&self, cfg: &Config, map: &str, save: &str, save_dir: &Path) -> Vec<BackupEntry> {
        list_zips(&cfg.backup_dir(map, save, save_dir).join(SNAPSHOT_DIR))
    }
}

fn list_zips(dir: &Path) -> Vec<BackupEntry> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "zip").unwrap_or(false) {
            if let Ok(meta) = entry.metadata() {
                out.push(BackupEntry {
                    filename: entry.file_name().to_string_lossy().to_string(),
                    size_mb: (meta.len() as f64 / 1048576.0 * 10.0).round() / 10.0,
                    time: fmt_st(meta.modified().unwrap_or(std::time::UNIX_EPOCH)),
                    path,
                });
            }
        }
    }
    out.sort_by(|a, b| b.time.cmp(&a.time));
    out
}

fn fmt_st(st: std::time::SystemTime) -> String {
    Engine::fmt_time(st)
}

// ===================== 恢复 =====================

impl Engine {
    /// 恢复存档（调用方持有 Engine 锁，与定时备份天然互斥）：
    /// 1. 检测游戏进程 → 2. 校验 zip 路径 → 3. 恢复前快照 →
    /// 4. 解压到同卷临时目录 → 5. 清空存档目录（保留备份目录）→ 6. 移入并记录清单
    pub fn restore_backup(
        &mut self,
        zip_path: &Path,
        map: &str,
        save: &str,
        save_dir: &Path,
    ) -> Result<(), RestoreError> {
        logger::info(&format!("开始恢复存档: {} -> {}", zip_path.display(), save_dir.display()));
        let cfg = self.config.lock().unwrap().clone();
        let out_dir = cfg.backup_dir(map, save, save_dir);

        if self.game_running() {
            return Err(RestoreError::GameRunning);
        }

        // 校验（在任何改动之前）
        let file = fs::File::open(zip_path).map_err(|e| RestoreError::Io(e.to_string()))?;
        let mut archive =
            zip::ZipArchive::new(file).map_err(|e| RestoreError::InvalidZip(e.to_string()))?;
        for i in 0..archive.len() {
            let name = archive.by_index_raw(i).map_err(|e| RestoreError::Io(e.to_string()))?.name().to_string();
            if !Self::safe_zip_name(&name) {
                return Err(RestoreError::InvalidZip(name));
            }
        }

        // 恢复前快照（安全网）
        let stamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
        self.backup_save(map, &format!("RESTORE_SNAPSHOT_{stamp}"), save_dir, "", true)
            .map_err(|e| RestoreError::Snapshot(e))?;

        // 解压到临时目录（与存档同卷，可整体移动）
        let tmp_extract = save_dir
            .parent()
            .unwrap_or(save_dir)
            .join(format!("_{save}_restore_tmp"));
        let _ = fs::remove_dir_all(&tmp_extract);
        fs::create_dir_all(&tmp_extract).map_err(|e| RestoreError::Io(e.to_string()))?;
        let extract_result = (|| -> Result<(), RestoreError> {
            for i in 0..archive.len() {
                let mut entry = archive
                    .by_index(i)
                    .map_err(|e| RestoreError::Io(e.to_string()))?;
                let rel = entry.name().replace('\\', "/");
                let dest = rel.split('/').fold(tmp_extract.clone(), |acc, seg| acc.join(seg));
                if entry.is_dir() {
                    fs::create_dir_all(&dest).map_err(|e| RestoreError::Io(e.to_string()))?;
                } else {
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent).map_err(|e| RestoreError::Io(e.to_string()))?;
                    }
                    let mut out = fs::File::create(&dest).map_err(|e| RestoreError::Io(e.to_string()))?;
                    io::copy(&mut entry, &mut out).map_err(|e| RestoreError::Io(e.to_string()))?;
                }
            }
            Ok(())
        })();
        if let Err(e) = extract_result {
            let _ = fs::remove_dir_all(&tmp_extract);
            logger::error(&format!("恢复失败: {e}"));
            return Err(e);
        }

        // 清空存档目录（保留备份目录）
        for item in fs::read_dir(save_dir).map_err(|e| RestoreError::Io(e.to_string()))?.flatten() {
            let path = item.path();
            let name = item.file_name().to_string_lossy().to_lowercase();
            if name == "backup" || Engine::is_under(&path, &out_dir) {
                continue;
            }
            let r = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
            if let Err(e) = r {
                logger::warn(&format!("删除失败 {}: {e}", path.display()));
            }
        }

        // 移入解压内容
        for item in fs::read_dir(&tmp_extract)
            .map_err(|e| RestoreError::Io(e.to_string()))?
            .flatten()
        {
            let dest = save_dir.join(item.file_name());
            if let Err(e) = fs::rename(item.path(), &dest) {
                logger::warn(&format!("移动失败 {} -> {}: {e}", item.path().display(), dest.display()));
            }
        }
        let _ = fs::remove_dir_all(&tmp_extract);

        // 记录恢复后的清单，避免恢复后立刻重复备份
        let manifest = Self::to_manifest(&Self::collect_files(save_dir, &out_dir));
        let mut cfg = self.config.lock().unwrap();
        cfg.manifests.insert(format!("{map}/{save}"), manifest);
        self.save_config(&cfg);

        logger::info(&format!("恢复完成: {}", zip_path.display()));
        Ok(())
    }
}

// ===================== 测试 =====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn setup(root: &Path) -> (Arc<Mutex<Config>>, PathBuf, Arc<Shared>, Engine) {
        let cfg_dir = root.join("cfg");
        let cfg = Config::load(&cfg_dir);
        std::fs::create_dir_all(root.join("Saves")).unwrap();
        let mut cfg = cfg;
        cfg.save_path = root.join("Saves").to_string_lossy().to_string();
        let cfg = Arc::new(Mutex::new(cfg));
        let shared = Arc::new(Shared {
            exit: AtomicBool::new(false),
            show_requested: AtomicBool::new(false),
            backup_now: AtomicBool::new(false),
            open_backup_folder: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            status: Mutex::new(String::new()),
            next_backup: Mutex::new(String::new()),
            notify_queue: Mutex::new(Default::default()),
            menu_ids: Mutex::new(Default::default()),
            dirty_scan: AtomicBool::new(false),
        });
        let engine = Engine::new(cfg.clone(), cfg_dir.clone(), shared.clone());
        (cfg, cfg_dir, shared, engine)
    }

    fn make_save(root: &Path) -> PathBuf {
        let save_dir = root.join("Saves").join("Map1").join("Save1");
        fs::create_dir_all(save_dir.join("Region")).unwrap();
        fs::write(save_dir.join("a.txt"), "hello").unwrap();
        fs::write(save_dir.join("Region").join("r.7rg"), b"x".repeat(100)).unwrap();
        save_dir
    }

    fn monitor(cfg: &Arc<Mutex<Config>>) {
        cfg.lock().unwrap().set_monitored("Map1/Save1", true);
    }

    #[test]
    fn scan_and_change_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, _, _, mut engine) = setup(tmp.path());
        let save_dir = make_save(tmp.path());
        monitor(&cfg);

        // 从未备份 → 变化
        assert_eq!(engine.check_saves_changed(&cfg.lock().unwrap()).len(), 1);
        // 备份后无变化
        engine.backup_save("Map1", "Save1", &save_dir, "Map1/Save1", false).unwrap();
        assert!(engine.check_saves_changed(&cfg.lock().unwrap()).is_empty());
        // 内容变化
        fs::write(save_dir.join("a.txt"), "hello world").unwrap();
        assert_eq!(engine.check_saves_changed(&cfg.lock().unwrap()).len(), 1);
        // 仅 mtime 变化（同内容）
        let f = save_dir.join("a.txt");
        fs::write(&f, "hello world").unwrap();
        filetime::set_file_mtime(&f, filetime::FileTime::from_unix_time(1_700_000_000, 0)).unwrap();
        assert_eq!(engine.check_saves_changed(&cfg.lock().unwrap()).len(), 1);
        // 删除文件
        fs::remove_file(save_dir.join("a.txt")).unwrap();
        assert_eq!(engine.check_saves_changed(&cfg.lock().unwrap()).len(), 1);
        // 未监控忽略
        cfg.lock().unwrap().set_monitored("Map1/Save1", false);
        assert!(engine.check_saves_changed(&cfg.lock().unwrap()).is_empty());
    }

    #[test]
    fn backup_zip_layout_and_no_tmp_leftover() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, _, _, mut engine) = setup(tmp.path());
        let save_dir = make_save(tmp.path());
        engine.backup_save("Map1", "Save1", &save_dir, "Map1/Save1", false).unwrap();

        let backup_dir = save_dir.join("backup");
        let zips: Vec<_> = fs::read_dir(&backup_dir).unwrap().flatten().collect();
        assert_eq!(zips.len(), 1);
        assert!(zips[0].file_name().to_string_lossy().starts_with("Map1_Save1_"));

        let f = fs::File::open(zips[0].path()).unwrap();
        let mut ar = zip::ZipArchive::new(f).unwrap();
        let names: Vec<String> = (0..ar.len()).map(|i| ar.by_index(i).unwrap().name().to_string()).collect();
        assert!(names.contains(&"a.txt".to_string()));
        assert!(names.contains(&"Region/r.7rg".to_string()));
        assert!(!names.iter().any(|n| n.starts_with("backup")));

        let map_dir = save_dir.parent().unwrap();
        assert!(fs::read_dir(map_dir).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".tmp")));
    }

    #[test]
    fn backup_to_custom_output() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, _, _, mut engine) = setup(tmp.path());
        let save_dir = make_save(tmp.path());
        let out_root = tmp.path().join("Backups");
        cfg.lock().unwrap().backup_output_path = out_root.to_string_lossy().to_string();

        engine.backup_save("Map1", "Save1", &save_dir, "Map1/Save1", false).unwrap();
        assert!(out_root.join("Map1").join("Save1").join("backup").is_dir() || {
            // 输出目录直接就是 out_root/Map1/Save1
            fs::read_dir(out_root.join("Map1").join("Save1")).unwrap().count() > 0
        });
        assert!(!save_dir.join("backup").exists());
        assert!(engine.check_saves_changed(&cfg.lock().unwrap()).is_empty());

        // 输出目录在存档树内部时也不得混入清单
        cfg.lock().unwrap().backup_output_path = save_dir.join("mybackups").to_string_lossy().to_string();
        engine.backup_save("Map1", "Save1", &save_dir, "Map1/Save1", false).unwrap();
        assert!(engine.check_saves_changed(&cfg.lock().unwrap()).is_empty());
    }

    #[test]
    fn snapshots_dir_and_retention() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, _, _, mut engine) = setup(tmp.path());
        let save_dir = make_save(tmp.path());
        for _ in 0..5 {
            engine.backup_save("Map1", "Save1", &save_dir, "", true).unwrap();
        }
        let snaps = save_dir.join("backup").join("snapshots");
        assert_eq!(fs::read_dir(&snaps).unwrap().flatten().count(), 3);
        assert!(cfg.lock().unwrap().manifests.get("Map1/Save1").is_none());
        assert!(engine.get_save_backups(&cfg.lock().unwrap(), "Map1", "Save1", &save_dir).is_empty());
    }

    fn make_zip(dir: &Path, name: &str, age_days: u64, offset_secs: u64, size: u64) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, vec![b'z'; size as usize]).unwrap();
        let delta = age_days as i64 * 86400 - offset_secs as i64;
        let now = std::time::SystemTime::now();
        let ts = if delta >= 0 {
            now - std::time::Duration::from_secs(delta as u64)
        } else {
            now + std::time::Duration::from_secs((-delta) as u64)
        };
        filetime::set_file_mtime(&p, filetime::FileTime::from_system_time(ts)).unwrap();
        p
    }

    #[test]
    fn retention_versions_daily_and_size_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, _, _, mut engine) = setup(tmp.path());
        let save_dir = tmp.path().join("Saves").join("Map1").join("Save1");
        fs::create_dir_all(&save_dir).unwrap();
        let backup_dir = save_dir.join("backup");
        fs::create_dir_all(&backup_dir).unwrap();

        // 今天 3 个（进入"最近 N"窗口），窗外：2 天前 2 个 / 1 天前 2 个 / 今天较早 1 个
        for i in 0..3 {
            make_zip(&backup_dir, &format!("today{i}.zip"), 0, (i as u64 + 1) * 60, 100);
        }
        make_zip(&backup_dir, "d2_b.zip", 2, 2 * 60, 100);
        make_zip(&backup_dir, "d2_a.zip", 2, 60, 100);
        make_zip(&backup_dir, "d1_b.zip", 1, 2 * 60, 100);
        make_zip(&backup_dir, "d1_a.zip", 1, 60, 100);
        make_zip(&backup_dir, "d0_old.zip", 0, 0, 100);

        {
            let mut c = cfg.lock().unwrap();
            c.max_backup_versions = 3;
            c.daily_keep_days = 2;
        }
        let deleted = engine.cleanup_old_backups("Map1", "Save1", &save_dir);
        let remaining: Vec<String> = fs::read_dir(&backup_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        // 保留：today×3（窗口）+ d0_old（尾部今天最新）+ d1_b（尾部昨天最新）；删除 3 个
        assert_eq!(deleted, 3, "remaining: {remaining:?}");
        for name in ["today0.zip", "today1.zip", "today2.zip", "d0_old.zip", "d1_b.zip"] {
            assert!(remaining.contains(&name.into()), "missing {name} in {remaining:?}");
        }
        for name in ["d2_b.zip", "d2_a.zip", "d1_a.zip"] {
            assert!(!remaining.contains(&name.into()), "unexpected {name} in {remaining:?}");
        }

        // 总量上限：5 个 × 1MB = 5MB，上限 2MB → 从最旧开始删到 2MB（剩 2 个）
        for e in fs::read_dir(&backup_dir).unwrap().flatten() {
            fs::remove_file(e.path()).unwrap();
        }
        for i in 0..5 {
            make_zip(&backup_dir, &format!("s{i}.zip"), 0, (i as u64 + 1) * 60, 1024 * 1024);
        }
        {
            let mut c = cfg.lock().unwrap();
            c.max_backup_versions = 10;
            c.daily_keep_days = 0;
            c.max_total_size_mb = 2;
        }
        let deleted = engine.cleanup_old_backups("Map1", "Save1", &save_dir);
        let remaining: Vec<String> = fs::read_dir(&backup_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(deleted, 3, "remaining: {remaining:?}");
        let mut remaining = remaining;
        remaining.sort();
        assert_eq!(remaining, vec!["s3.zip".to_string(), "s4.zip".to_string()]);
    }

    #[test]
    fn restore_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, _, _, mut engine) = setup(tmp.path());
        let save_dir = make_save(tmp.path());
        monitor(&cfg);

        engine.backup_save("Map1", "Save1", &save_dir, "Map1/Save1", false).unwrap();
        let v1 = engine.get_save_backups(&cfg.lock().unwrap(), "Map1", "Save1", &save_dir)[0].path.clone();

        fs::write(save_dir.join("a.txt"), "v2").unwrap();
        fs::write(save_dir.join("extra.txt"), "added").unwrap();
        engine.backup_save("Map1", "Save1", &save_dir, "Map1/Save1", false).unwrap();

        engine.restore_backup(&v1, "Map1", "Save1", &save_dir).unwrap();
        assert_eq!(fs::read_to_string(save_dir.join("a.txt")).unwrap(), "hello");
        assert!(!save_dir.join("extra.txt").exists());
        // 快照已创建
        assert!(fs::read_dir(save_dir.join("backup").join("snapshots")).unwrap().flatten().count() >= 1);
        // 备份目录保留
        assert!(save_dir.join("backup").is_dir());
        // 恢复后不立刻重复备份
        assert!(engine.check_saves_changed(&cfg.lock().unwrap()).is_empty());
    }

    #[test]
    fn restore_rejects_zip_slip() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, _, _, mut engine) = setup(tmp.path());
        let save_dir = make_save(tmp.path());
        engine.backup_save("Map1", "Save1", &save_dir, "Map1/Save1", false).unwrap();

        let bad = tmp.path().join("evil.zip");
        {
            let f = fs::File::create(&bad).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default();
            zw.start_file("../evil.txt", opts).unwrap();
            io::Write::write_all(&mut zw, b"boom").unwrap();
            zw.finish().unwrap();
        }
        assert!(matches!(
            engine.restore_backup(&bad, "Map1", "Save1", &save_dir),
            Err(RestoreError::InvalidZip(_))
        ));
        assert!(!tmp.path().join("evil.txt").exists());
        assert_eq!(fs::read_to_string(save_dir.join("a.txt")).unwrap(), "hello");
    }

    #[test]
    fn cleanup_only_after_new_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, _, _, mut engine) = setup(tmp.path());
        let save_dir = make_save(tmp.path());
        monitor(&cfg);
        // 首次备份，建立清单
        engine.run_backup();

        // 在备份目录塞 15 个"超限"的假旧备份：没有新备份产生时，绝不清理
        let backup_dir = save_dir.join("backup");
        for i in 0..15 {
            fs::write(backup_dir.join(format!("old{i}.zip")), b"old").unwrap();
        }
        let before = fs::read_dir(&backup_dir).unwrap().flatten().count();
        let status = engine.run_backup();
        assert_eq!(status.cleaned, 0, "没有新备份时不允许清理");
        let after = fs::read_dir(&backup_dir).unwrap().flatten().count();
        assert_eq!(before, after);

        // 一旦产生新备份，才按保留策略清理旧备份
        {
            let mut c = cfg.lock().unwrap();
            c.max_backup_versions = 5;
            c.daily_keep_days = 0;
        }
        fs::write(save_dir.join("a.txt"), "changed").unwrap();
        let status2 = engine.run_backup();
        assert!(status2.cleaned > 0, "有新备份后应清理旧备份");
        // 新备份 + 保留窗口内，绝不会全部删光
        let remain = fs::read_dir(&backup_dir).unwrap().flatten().count();
        assert!(remain >= 2 && remain <= 6, "remain={remain}");
        // 最新的一份永远还在
        assert!(fs::read_dir(&backup_dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(&Local::now().format("%Y%m%d").to_string())));
    }

    #[test]
    fn delete_backup_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, _, _, mut engine) = setup(tmp.path());
        let save_dir = make_save(tmp.path());
        engine.backup_save("Map1", "Save1", &save_dir, "Map1/Save1", false).unwrap();
        let zip = engine.get_save_backups(&cfg.lock().unwrap(), "Map1", "Save1", &save_dir)[0]
            .path
            .clone();
        assert!(zip.exists());
        engine.delete_backup_file(&zip).unwrap();
        assert!(!zip.exists());
        // 删除的是备份包本身，存档目录未变 → 不触发重新备份
        assert!(engine.check_saves_changed(&cfg.lock().unwrap()).is_empty());
    }

    #[test]
    fn tmp_files_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, _, _, mut engine) = setup(tmp.path());
        let map_dir = tmp.path().join("Saves").join("Map1");
        let save_dir = map_dir.join("Save1");
        fs::create_dir_all(&save_dir).unwrap();
        fs::write(map_dir.join("_Map1_Save1_2099.zip.tmp"), b"partial").unwrap();
        fs::create_dir_all(map_dir.join("_Save1_restore_tmp")).unwrap();
        fs::write(save_dir.join("a.txt"), "keep").unwrap();

        engine.cleanup_tmp_files();
        assert!(!map_dir.join("_Map1_Save1_2099.zip.tmp").exists());
        assert!(!map_dir.join("_Save1_restore_tmp").exists());
        assert!(save_dir.join("a.txt").exists());
    }
}
