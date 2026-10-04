"""BackupEngine：扫描、清单变更检测、备份、自定义输出位置、快照"""
import os

from app.config import ConfigManager
from app.backup_engine import BackupEngine


def make_env(tmp_path):
    """搭建：Saves/Map1/Save1 存档目录 + 配置 + 引擎"""
    saves_root = tmp_path / "Saves"
    save_dir = saves_root / "Map1" / "Save1"
    save_dir.mkdir(parents=True)
    (save_dir / "a.txt").write_text("hello", encoding="utf-8")
    region = save_dir / "Region"
    region.mkdir()
    (region / "r.7rg").write_bytes(b"x" * 100)

    cfg = ConfigManager(config_dir=str(tmp_path / "cfg"))
    cfg.set("save_path", str(saves_root))
    cfg.set_monitored("Map1/Save1", True)
    engine = BackupEngine(cfg)
    return cfg, engine, saves_root, save_dir


class TestScan:
    def test_scan_finds_save(self, tmp_path):
        _, engine, _, save_dir = make_env(tmp_path)
        saves = engine.scan_saves()
        assert len(saves) == 1
        map_name, save_name, path, key = saves[0]
        assert (map_name, save_name, key) == ("Map1", "Save1", "Map1/Save1")
        assert path == str(save_dir)

    def test_scan_missing_root(self, tmp_path):
        cfg = ConfigManager(config_dir=str(tmp_path / "cfg"))
        cfg.set("save_path", str(tmp_path / "nonexistent"))
        engine = BackupEngine(cfg)
        assert engine.scan_saves() == []


class TestChangeDetection:
    def test_never_backed_up_is_changed(self, tmp_path):
        _, engine, _, _ = make_env(tmp_path)
        changed = engine.check_saves_changed()
        assert [c[3] for c in changed] == ["Map1/Save1"]

    def test_unchanged_after_backup(self, tmp_path):
        _, engine, _, save_dir = make_env(tmp_path)
        engine.backup_save("Map1", "Save1", str(save_dir), "Map1/Save1")
        assert engine.check_saves_changed() == []

    def test_mtime_change_detected(self, tmp_path):
        _, engine, _, save_dir = make_env(tmp_path)
        engine.backup_save("Map1", "Save1", str(save_dir), "Map1/Save1")
        # 同内容但 mtime 变化（时钟方式无法可靠捕捉的场景）
        target = save_dir / "a.txt"
        st = os.stat(target)
        os.utime(target, ns=(st.st_atime_ns, st.st_mtime_ns + 10**9))
        assert engine.check_saves_changed() != []

    def test_size_change_detected(self, tmp_path):
        _, engine, _, save_dir = make_env(tmp_path)
        engine.backup_save("Map1", "Save1", str(save_dir), "Map1/Save1")
        (save_dir / "a.txt").write_text("hello world", encoding="utf-8")
        assert engine.check_saves_changed() != []

    def test_deleted_file_detected(self, tmp_path):
        _, engine, _, save_dir = make_env(tmp_path)
        engine.backup_save("Map1", "Save1", str(save_dir), "Map1/Save1")
        os.remove(save_dir / "a.txt")
        assert engine.check_saves_changed() != []

    def test_new_file_detected(self, tmp_path):
        _, engine, _, save_dir = make_env(tmp_path)
        engine.backup_save("Map1", "Save1", str(save_dir), "Map1/Save1")
        (save_dir / "new.txt").write_text("n", encoding="utf-8")
        assert engine.check_saves_changed() != []

    def test_unmonitored_ignored(self, tmp_path):
        cfg, engine, _, save_dir = make_env(tmp_path)
        cfg.set_monitored("Map1/Save1", False)
        assert engine.check_saves_changed() == []


class TestBackup:
    def test_backup_creates_zip_in_default_dir(self, tmp_path):
        _, engine, _, save_dir = make_env(tmp_path)
        ok, skipped = engine.backup_save("Map1", "Save1", str(save_dir), "Map1/Save1")
        assert ok and skipped == 0
        backup_dir = save_dir / "backup"
        zips = [f for f in os.listdir(backup_dir) if f.endswith(".zip")]
        assert len(zips) == 1
        assert zips[0].startswith("Map1_Save1_")

    def test_backup_zip_contains_files_but_not_backup_dir(self, tmp_path):
        import zipfile
        _, engine, _, save_dir = make_env(tmp_path)
        engine.backup_save("Map1", "Save1", str(save_dir), "Map1/Save1")
        backup_dir = save_dir / "backup"
        zip_path = backup_dir / [f for f in os.listdir(backup_dir) if f.endswith(".zip")][0]
        with zipfile.ZipFile(zip_path) as zf:
            names = set(zf.namelist())
        assert "a.txt" in names and "Region/r.7rg" in names
        assert not any("backup" in n.split("/")[0] for n in names)

    def test_backup_to_custom_output(self, tmp_path):
        cfg, engine, saves_root, save_dir = make_env(tmp_path)
        out_root = tmp_path / "Backups"
        cfg.set("backup_output_path", str(out_root))
        ok, _ = engine.backup_save("Map1", "Save1", str(save_dir), "Map1/Save1")
        assert ok
        # zip 落在 {自定义}/{Map}/{Save} 下，且存档目录内没有 backup 文件夹
        zip_dir = out_root / "Map1" / "Save1"
        assert any(f.endswith(".zip") for f in os.listdir(zip_dir))
        assert not (save_dir / "backup").exists()

    def test_custom_output_inside_save_tree_excluded(self, tmp_path):
        """备份目录在存档树内部时，其内容不得混入 zip 与清单（否则每轮都误判变化）"""
        cfg, engine, _, save_dir = make_env(tmp_path)
        # 把输出位置设为存档目录内部的子文件夹（backup_dir = .../Save1/mybackups/Map1/Save1）
        cfg.set("backup_output_path", str(save_dir / "mybackups"))
        ok, _ = engine.backup_save("Map1", "Save1", str(save_dir), "Map1/Save1")
        assert ok
        assert engine.check_saves_changed() == []

    def test_no_tmp_leftover(self, tmp_path):
        _, engine, _, save_dir = make_env(tmp_path)
        engine.backup_save("Map1", "Save1", str(save_dir), "Map1/Save1")
        map_dir = save_dir.parent
        leftovers = [f for f in os.listdir(map_dir) if f.endswith(".tmp")]
        assert leftovers == []

    def test_get_save_backups(self, tmp_path):
        _, engine, _, save_dir = make_env(tmp_path)
        engine.backup_save("Map1", "Save1", str(save_dir), "Map1/Save1")
        backups = engine.get_save_backups("Map1", "Save1", str(save_dir))
        assert len(backups) == 1
        assert backups[0]["size_mb"] >= 0


class TestSnapshot:
    def test_snapshot_goes_to_snapshots_dir(self, tmp_path):
        cfg, engine, _, save_dir = make_env(tmp_path)
        ok, _ = engine.backup_save("Map1", "Save1", str(save_dir), "", snapshot=True)
        assert ok
        snaps = save_dir / "backup" / "snapshots"
        assert any(f.endswith(".zip") for f in os.listdir(snaps))
        # 快照不记录清单（不算正式备份）
        assert cfg.get_manifest("Map1/Save1") is None
        # 不出现在普通备份列表
        assert engine.get_save_backups("Map1", "Save1", str(save_dir)) == []

    def test_snapshot_retention(self, tmp_path):
        _, engine, _, save_dir = make_env(tmp_path)
        for _ in range(5):
            ok, _ = engine.backup_save("Map1", "Save1", str(save_dir), "", snapshot=True)
            assert ok
        snaps_dir = save_dir / "backup" / "snapshots"
        assert len([f for f in os.listdir(snaps_dir) if f.endswith(".zip")]) == 3


class TestRunBackup:
    def test_run_backup_status(self, tmp_path):
        _, engine, _, save_dir = make_env(tmp_path)
        status = engine.run_backup()
        assert status["total"] == 1
        assert status["backed_up"] == 1
        assert status["failed"] == 0
        assert "备份完成" in status["message"]

    def test_run_backup_no_change(self, tmp_path):
        _, engine, _, save_dir = make_env(tmp_path)
        engine.run_backup()
        status = engine.run_backup()
        assert status["total"] == 0
        assert "没有检测到变化" in status["message"]
