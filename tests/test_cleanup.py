"""备份清理：版本轮转 + 每日分层保留"""
import os
import time

from app.config import ConfigManager
from app.backup_engine import BackupEngine


def save_dir_path(tmp_path):
    return tmp_path / "Saves" / "Map1" / "Save1"


def make_env(tmp_path, max_versions=10, daily_keep=0):
    save_dir = save_dir_path(tmp_path)
    save_dir.mkdir(parents=True)
    cfg = ConfigManager(config_dir=str(tmp_path / "cfg"))
    cfg.set("save_path", str(tmp_path / "Saves"))
    cfg.set("max_backup_versions", max_versions)
    cfg.set("daily_keep_days", daily_keep)
    engine = BackupEngine(cfg)
    backup_dir = save_dir / "backup"
    backup_dir.mkdir()
    return engine, backup_dir


def create_zip(backup_dir, name, ts):
    p = backup_dir / name
    p.write_bytes(b"zip")
    os.utime(p, (ts, ts))
    return p


class TestVersionRotation:
    def test_keeps_newest_n(self, tmp_path):
        engine, backup_dir = make_env(tmp_path, max_versions=10, daily_keep=0)
        base = time.time() - 20 * 86400
        for i in range(15):
            create_zip(backup_dir, f"b{i:02d}.zip", base + i * 3600)

        deleted = engine.cleanup_old_backups("Map1", "Save1", str(save_dir_path(tmp_path)))
        assert deleted == 5
        remaining = sorted(p.name for p in backup_dir.glob("*.zip"))
        assert len(remaining) == 10
        # 保留最新的 10 个
        assert "b14.zip" in remaining and "b00.zip" not in remaining


class TestDailyKeep:
    def test_daily_layer_keeps_newest_per_day(self, tmp_path):
        engine, backup_dir = make_env(tmp_path, max_versions=3, daily_keep=2)
        base = int(time.time()) - 10 * 86400

        # 今天 3 个（进入"最近 N"窗口）
        for i in range(3):
            create_zip(backup_dir, f"today{i}.zip", base + 10 * 86400 + i)
        # day2: 2 个 / day1: 2 个 / day0: 1 个（都在窗口外，按天分层）
        days = {2: ["d2_a.zip", "d2_b.zip"], 1: ["d1_a.zip", "d1_b.zip"], 0: ["d0_a.zip"]}
        for day, names in days.items():
            for j, name in enumerate(names):
                create_zip(backup_dir, name, base + day * 86400 + j * 60)

        deleted = engine.cleanup_old_backups("Map1", "Save1", str(save_dir_path(tmp_path)))
        remaining = sorted(p.name for p in backup_dir.glob("*.zip"))

        # 保留：today 3 个 + day2 最新 1 个（d2_b）+ day1 最新 1 个（d1_b）；删除 3 个
        assert deleted == 3
        assert set(remaining) == {"today0.zip", "today1.zip", "today2.zip",
                                  "d2_b.zip", "d1_b.zip"}

    def test_daily_keep_disabled(self, tmp_path):
        engine, backup_dir = make_env(tmp_path, max_versions=2, daily_keep=0)
        base = time.time() - 5 * 86400
        for i in range(5):
            create_zip(backup_dir, f"b{i}.zip", base + i * 3600)
        engine.cleanup_old_backups("Map1", "Save1", str(save_dir_path(tmp_path)))
        assert len(list(backup_dir.glob("*.zip"))) == 2

    def test_noop_when_under_limit(self, tmp_path):
        engine, backup_dir = make_env(tmp_path, max_versions=10, daily_keep=0)
        for i in range(3):
            create_zip(backup_dir, f"b{i}.zip", time.time() + i)
        assert engine.cleanup_old_backups("Map1", "Save1", str(save_dir_path(tmp_path))) == 0


class TestTmpCleanup:
    def test_cleanup_tmp_files(self, tmp_path):
        saves_root = tmp_path / "Saves"
        map_dir = saves_root / "Map1"
        save_dir = map_dir / "Save1"
        save_dir.mkdir(parents=True)
        cfg = ConfigManager(config_dir=str(tmp_path / "cfg"))
        cfg.set("save_path", str(saves_root))
        engine = BackupEngine(cfg)

        stale = map_dir / "_Map1_Save1_2099.zip.tmp"
        stale.write_bytes(b"partial")
        stale_restore = map_dir / "_Save1_restore_tmp"
        stale_restore.mkdir()
        keep = save_dir / "a.txt"
        keep.write_text("keep", encoding="utf-8")

        engine.cleanup_tmp_files()
        assert not stale.exists()
        assert not stale_restore.exists()
        assert keep.exists()
