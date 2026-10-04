//! 配置管理：%APPDATA%\7DaysBackup\config.json
//! - 原子写入（临时文件 + rename）
//! - 兼容旧版 Python 配置（所有字段带 serde default）
//! - 文件清单（manifests）用于精确的存档变更检测

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub fn default_config_dir() -> PathBuf {
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs_fallback());
    base.join("7DaysBackup")
}

fn dirs_fallback() -> PathBuf {
    std::env::var("USERPROFILE")
        .map(|p| PathBuf::from(p).join("AppData").join("Roaming"))
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn default_saves_dir() -> PathBuf {
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs_fallback());
    base.join("7DaysToDie").join("Saves")
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(default = "d_interval")]
    pub backup_interval_seconds: u64,
    #[serde(default = "d_versions")]
    pub max_backup_versions: u32,
    #[serde(default = "d_daily_keep")]
    pub daily_keep_days: u32,
    /// 备份总量上限（MB），0 = 不限
    #[serde(default)]
    pub max_total_size_mb: u64,
    /// zip 压缩级别 1-9
    #[serde(default = "d_compression")]
    pub compression_level: u32,
    #[serde(default)]
    pub save_path: String,
    /// 备份输出位置，空 = 各存档目录内 backup 文件夹
    #[serde(default)]
    pub backup_output_path: String,
    #[serde(default = "d_true")]
    pub minimize_to_tray: bool,
    #[serde(default = "d_true")]
    pub auto_start: bool,
    #[serde(default)]
    pub monitored_saves: Vec<String>,
    #[serde(default)]
    pub last_backup_times: HashMap<String, String>,
    #[serde(default = "d_false")]
    pub monitored_initialized: bool,
    /// 各存档上次备份时的文件清单 {map/save: {relpath: [mtime_nanos, size]}}
    #[serde(default)]
    pub manifests: HashMap<String, HashMap<String, [u64; 2]>>,
}

fn d_interval() -> u64 { 300 }
fn d_versions() -> u32 { 10 }
fn d_daily_keep() -> u32 { 7 }
fn d_compression() -> u32 { 6 }
fn d_true() -> bool { true }
fn d_false() -> bool { false }

impl Default for Config {
    fn default() -> Self {
        Self {
            backup_interval_seconds: d_interval(),
            max_backup_versions: d_versions(),
            daily_keep_days: d_daily_keep(),
            max_total_size_mb: 0,
            compression_level: d_compression(),
            save_path: String::new(),
            backup_output_path: String::new(),
            minimize_to_tray: true,
            auto_start: true,
            monitored_saves: Vec::new(),
            last_backup_times: HashMap::new(),
            monitored_initialized: false,
            manifests: HashMap::new(),
        }
    }
}

impl Config {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("config.json");
        let mut need_save = !path.exists();
        let mut cfg = if path.exists() {
            fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<Config>(&s).ok())
                .unwrap_or_else(|| {
                    crate::logger::warn("配置文件损坏，使用默认配置");
                    need_save = true;
                    Config::default()
                })
        } else {
            Config::default()
        };

        // 迁移：旧版 Python 程序把文件清单存在单独的 manifests.json，合并进来
        if cfg.manifests.is_empty() {
            let legacy = dir.join("manifests.json");
            if legacy.exists() {
                if let Ok(text) = fs::read_to_string(&legacy) {
                    if let Ok(m) = serde_json::from_str::<HashMap<String, HashMap<String, [u64; 2]>>>(&text)
                    {
                        if !m.is_empty() {
                            crate::logger::info(&format!(
                                "已从 manifests.json 迁移 {} 个存档的文件清单",
                                m.len()
                            ));
                            cfg.manifests = m;
                            need_save = true;
                        }
                    }
                }
            }
        }

        if need_save {
            cfg.save(dir);
        }
        cfg
    }

    /// 原子写入：先写 .tmp 再 rename，任一时刻磁盘上的文件都是完整的
    pub fn save(&self, dir: &Path) {
        let _ = fs::create_dir_all(dir);
        let path = dir.join("config.json");
        let tmp = dir.join("config.json.tmp");
        if let Ok(json) = serde_json::to_string_pretty(self) {
            if fs::write(&tmp, json).is_ok() {
                let _ = fs::rename(&tmp, &path);
            }
        }
    }

    pub fn save_path(&self) -> PathBuf {
        if self.save_path.trim().is_empty() {
            default_saves_dir()
        } else {
            PathBuf::from(self.save_path.trim())
        }
    }

    /// 备份输出目录：自定义位置 → {自定义}/{Map}/{Save}；否则 {存档目录}/backup
    pub fn backup_dir(&self, map: &str, save: &str, save_dir: &Path) -> PathBuf {
        let custom = self.backup_output_path.trim();
        if custom.is_empty() {
            save_dir.join("backup")
        } else {
            PathBuf::from(custom).join(map).join(save)
        }
    }

    pub fn is_monitored(&self, key: &str) -> bool {
        self.monitored_saves.iter().any(|k| k == key)
    }

    pub fn set_monitored(&mut self, key: &str, monitored: bool) {
        if monitored {
            if !self.is_monitored(key) {
                self.monitored_saves.push(key.to_string());
            }
        } else {
            self.monitored_saves.retain(|k| k != key);
        }
    }

    /// 清理已删除存档的残留条目（manifests / last_backup_times / monitored_saves）。
    /// 返回是否有变更。
    pub fn prune_stale_entries(&mut self, root: &Path, present_keys: &[String]) -> bool {
        if !root.is_dir() {
            return false; // 根目录不可见（如网络盘临时断联）时不做清理，避免误删
        }
        let stale: Vec<String> = self
            .last_backup_times
            .keys()
            .chain(self.manifests.keys())
            .chain(self.monitored_saves.iter())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|key| !present_keys.contains(key))
            .filter(|key| {
                let parts: Vec<&str> = key.splitn(2, '/').collect();
                if parts.len() != 2 {
                    return false;
                }
                let dir = root.join(parts[0]).join(parts[1]);
                !dir.is_dir() // 目录还在时保守起见不清理（重命名/临时隐藏等场景）
            })
            .collect();
        if stale.is_empty() {
            return false;
        }
        for key in &stale {
            self.last_backup_times.remove(key);
            self.manifests.remove(key);
            self.monitored_saves.retain(|k| k != key);
        }
        crate::logger::info(&format!("已清理 {} 个失效存档的残留配置", stale.len()));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = Config::load(tmp.path());
        assert_eq!(cfg.backup_interval_seconds, 300);
        assert_eq!(cfg.max_total_size_mb, 0);
        assert_eq!(cfg.compression_level, 6);
        assert!(cfg.auto_start);

        let mut cfg2 = cfg.clone();
        cfg2.max_total_size_mb = 4096;
        cfg2.set_monitored("Map/Save1", true);
        cfg2.save(tmp.path());
        let cfg3 = Config::load(tmp.path());
        assert_eq!(cfg3.max_total_size_mb, 4096);
        assert!(cfg3.is_monitored("Map/Save1"));
        assert!(!tmp.path().join("config.json.tmp").exists());
    }

    #[test]
    fn legacy_config_missing_fields_get_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("config.json"),
            r#"{"backup_interval_seconds": 600}"#,
        )
        .unwrap();
        let cfg = Config::load(tmp.path());
        assert_eq!(cfg.backup_interval_seconds, 600);
        assert_eq!(cfg.daily_keep_days, 7);
        assert!(cfg.manifests.is_empty());
    }

    #[test]
    fn corrupt_config_falls_back() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("config.json"), "{ truncated").unwrap();
        let cfg = Config::load(tmp.path());
        assert_eq!(cfg.backup_interval_seconds, 300);
    }

    #[test]
    fn migrates_legacy_manifests_json() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = r#"{"Map1/Save1": {"a.txt": [1759500000123456789, 123]}}"#;
        std::fs::write(tmp.path().join("manifests.json"), legacy).unwrap();
        let cfg = Config::load(tmp.path());
        assert!(cfg.manifests.contains_key("Map1/Save1"));
        // 迁移结果已写回 config.json，二次加载不再重复迁移
        let cfg2 = Config::load(tmp.path());
        assert!(cfg2.manifests.contains_key("Map1/Save1"));
    }

    #[test]
    fn backup_dir_custom_and_default() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        let save_dir = tmp.path().join("Map1").join("Save1");
        assert_eq!(cfg.backup_dir("Map1", "Save1", &save_dir), save_dir.join("backup"));
        cfg.backup_output_path = "D:/Backups".into();
        assert_eq!(
            cfg.backup_dir("Map1", "Save1", &save_dir),
            PathBuf::from("D:/Backups").join("Map1").join("Save1")
        );
    }
}
