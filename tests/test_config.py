"""ConfigManager：原子写入、默认值合并、清单存储"""
import json
import os

from app.config import ConfigManager


def make_config(tmp_path):
    return ConfigManager(config_dir=str(tmp_path / "cfg"))


class TestConfigBasics:
    def test_creates_default_config(self, tmp_path):
        cfg = make_config(tmp_path)
        assert cfg.get("backup_interval_seconds") == 300
        assert cfg.get("max_backup_versions") == 10
        assert cfg.get("daily_keep_days") == 7
        assert cfg.get("backup_output_path") == ""
        assert os.path.exists(cfg.config_dir)

    def test_set_persists(self, tmp_path):
        cfg = make_config(tmp_path)
        cfg.set("max_backup_versions", 25)
        # 重新实例化（同一目录）应读到持久化的值
        cfg2 = ConfigManager(config_dir=cfg.config_dir)
        assert cfg2.get("max_backup_versions") == 25

    def test_merges_new_default_fields(self, tmp_path):
        cfg = make_config(tmp_path)
        # 模拟旧版本配置文件：缺少新增字段
        with open(os.path.join(cfg.config_dir, "config.json"), "w", encoding="utf-8") as f:
            json.dump({"backup_interval_seconds": 600}, f)
        cfg2 = ConfigManager(config_dir=cfg.config_dir)
        assert cfg2.get("backup_interval_seconds") == 600
        assert cfg2.get("daily_keep_days") == 7  # 新字段自动补充

    def test_corrupt_config_falls_back_to_defaults(self, tmp_path):
        cfg = make_config(tmp_path)
        with open(os.path.join(cfg.config_dir, "config.json"), "w", encoding="utf-8") as f:
            f.write("{ truncated json...")
        cfg2 = ConfigManager(config_dir=cfg.config_dir)
        assert cfg2.get("backup_interval_seconds") == 300

    def test_atomic_write_no_tmp_leftover(self, tmp_path):
        cfg = make_config(tmp_path)
        cfg.set("auto_start", False)
        cfg_path = os.path.join(cfg.config_dir, "config.json")
        assert os.path.exists(cfg_path)
        assert not os.path.exists(cfg_path + ".tmp")
        with open(cfg_path, encoding="utf-8") as f:
            assert json.load(f)["auto_start"] is False

    def test_monitored_management(self, tmp_path):
        cfg = make_config(tmp_path)
        assert not cfg.is_monitored("Map/Save1")
        cfg.set_monitored("Map/Save1", True)
        assert cfg.is_monitored("Map/Save1")
        cfg.set_monitored("Map/Save1", False)
        assert not cfg.is_monitored("Map/Save1")


class TestManifest:
    def test_manifest_none_when_never_backed_up(self, tmp_path):
        cfg = make_config(tmp_path)
        assert cfg.get_manifest("Map/Save1") is None

    def test_manifest_persists_and_roundtrips(self, tmp_path):
        cfg = make_config(tmp_path)
        manifest = {"a.txt": [1759500000123456789, 123], "Region/r.rgi": [1, 2]}
        cfg.set_manifest("Map/Save1", manifest)
        cfg2 = ConfigManager(config_dir=cfg.config_dir)
        assert cfg2.get_manifest("Map/Save1") == manifest

    def test_manifest_file_atomic(self, tmp_path):
        cfg = make_config(tmp_path)
        cfg.set_manifest("Map/Save1", {"a.txt": [1, 2]})
        p = os.path.join(cfg.config_dir, "manifests.json")
        assert os.path.exists(p)
        assert not os.path.exists(p + ".tmp")
