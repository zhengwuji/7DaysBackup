r"""
配置管理模块
负责读写 %APPDATA%\7DaysBackup\config.json
"""
import os
import json
import threading
import logging

logger = logging.getLogger("BackupTool")


class ConfigManager:
    """线程安全的配置管理器"""

    DEFAULT_CONFIG = {
        "backup_interval_seconds": 300,     # 备份间隔，默认300秒（5分钟）
        "max_backup_versions": 10,          # 最大保留版本数
        "save_path": "",                    # 存档路径（空则使用默认路径）
        "minimize_to_tray": True,           # 关闭窗口时最小化到托盘
        "auto_start": True,                 # 开机自启（默认开启）
        "monitored_saves": [],              # 勾选的存档列表 ["MapName/SaveName", ...]
        "last_backup_times": {},            # 各存档上次备份时间 {map/save: iso_str}
        "monitored_initialized": False,     # 是否已完成首次全选初始化
    }

    def __init__(self):
        self._lock = threading.Lock()
        self._config_dir = os.path.join(os.getenv("APPDATA"), "7DaysBackup")
        self._config_path = os.path.join(self._config_dir, "config.json")
        self._config = {}
        self._load()

    # ---------- 路径 ----------
    @property
    def config_dir(self):
        return self._config_dir

    @property
    def save_path(self):
        path = self.get("save_path")
        if not path:
            path = os.path.join(os.getenv("APPDATA"), "7DaysToDie", "Saves")
        return path

    # ---------- 读写 ----------
    def _load(self):
        """从文件加载配置，不存在则创建默认"""
        try:
            if os.path.exists(self._config_path):
                with open(self._config_path, "r", encoding="utf-8") as f:
                    loaded = json.load(f)
                # 合并默认值（新字段自动补充）
                self._config = {**self.DEFAULT_CONFIG, **loaded}
            else:
                self._config = dict(self.DEFAULT_CONFIG)
                self._save()
        except Exception as e:
            logger.warning("加载配置失败，使用默认配置: %s", e)
            self._config = dict(self.DEFAULT_CONFIG)

    def _save(self):
        """保存配置到文件"""
        try:
            os.makedirs(self._config_dir, exist_ok=True)
            with open(self._config_path, "w", encoding="utf-8") as f:
                json.dump(self._config, f, indent=2, ensure_ascii=False)
        except Exception as e:
            logger.error("保存配置失败: %s", e)

    def get(self, key, default=None):
        """读取配置项"""
        with self._lock:
            return self._config.get(key, default)

    def set(self, key, value):
        """写入配置项并持久化"""
        with self._lock:
            self._config[key] = value
            self._save()

    def update(self, d: dict):
        """批量更新配置项"""
        with self._lock:
            self._config.update(d)
            self._save()

    # ---------- 备份时间记录 ----------
    def get_last_backup_time(self, relative_path: str):
        """获取某个存档的上次备份时间，无记录则返回 None"""
        with self._lock:
            return self._config["last_backup_times"].get(relative_path)

    def set_last_backup_time(self, relative_path: str, iso_str: str):
        """记录某个存档的备份时间"""
        with self._lock:
            self._config["last_backup_times"][relative_path] = iso_str
            self._save()

    def clear_backup_times(self):
        """清除所有备份时间记录"""
        with self._lock:
            self._config["last_backup_times"] = {}
            self._save()

    # ---------- 监控存档管理 ----------
    def is_monitored(self, relative_path: str) -> bool:
        """检查存档是否被勾选监控"""
        with self._lock:
            return relative_path in self._config.get("monitored_saves", [])

    def set_monitored(self, relative_path: str, monitored: bool):
        """设置存档的监控状态"""
        with self._lock:
            monitored_list = self._config.get("monitored_saves", [])
            if monitored and relative_path not in monitored_list:
                monitored_list.append(relative_path)
            elif not monitored and relative_path in monitored_list:
                monitored_list.remove(relative_path)
            self._config["monitored_saves"] = monitored_list
            self._save()

    def get_monitored_saves(self) -> list:
        """获取所有被监控的存档列表"""
        with self._lock:
            return list(self._config.get("monitored_saves", []))
