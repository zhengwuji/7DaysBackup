"""恢复备份：往返一致性、zip 路径穿越防护、恢复后不重复备份"""
import os
import zipfile

from app.config import ConfigManager
from app.backup_engine import BackupEngine


def make_env(tmp_path):
    saves_root = tmp_path / "Saves"
    save_dir = saves_root / "Map1" / "Save1"
    (save_dir / "Region").mkdir(parents=True)
    (save_dir / "a.txt").write_text("v1", encoding="utf-8")
    (save_dir / "Region" / "r.7rg").write_bytes(b"region-v1")

    cfg = ConfigManager(config_dir=str(tmp_path / "cfg"))
    cfg.set("save_path", str(saves_root))
    cfg.set_monitored("Map1/Save1", True)
    engine = BackupEngine(cfg)
    return cfg, engine, save_dir


def get_latest_zip(engine, save_dir):
    backups = engine.get_save_backups("Map1", "Save1", str(save_dir))
    assert backups
    return backups[0]["path"]


class TestRestoreRoundtrip:
    def test_restore_old_version(self, tmp_path):
        _, engine, save_dir = make_env(tmp_path)

        # 备份 v1
        engine.backup_save("Map1", "Save1", str(save_dir), "Map1/Save1")
        v1_zip = get_latest_zip(engine, save_dir)

        # 存档演进到 v2
        (save_dir / "a.txt").write_text("v2", encoding="utf-8")
        (save_dir / "Region" / "r.7rg").write_bytes(b"region-v2")
        (save_dir / "extra.txt").write_text("added", encoding="utf-8")
        engine.backup_save("Map1", "Save1", str(save_dir), "Map1/Save1")

        # 恢复 v1
        assert engine.restore_backup(v1_zip, "Map1", "Save1", str(save_dir)) is True
        assert (save_dir / "a.txt").read_text(encoding="utf-8") == "v1"
        assert (save_dir / "Region" / "r.7rg").read_bytes() == b"region-v1"
        assert not (save_dir / "extra.txt").exists()

        # 恢复前快照已创建
        snaps = save_dir / "backup" / "snapshots"
        assert any(f.endswith(".zip") for f in os.listdir(snaps))

        # 恢复内容与清单一致 -> 不会被立刻重复备份
        assert engine.check_saves_changed() == []

    def test_restore_preserves_backup_dir(self, tmp_path):
        _, engine, save_dir = make_env(tmp_path)
        engine.backup_save("Map1", "Save1", str(save_dir), "Map1/Save1")
        zip_path = get_latest_zip(engine, save_dir)
        assert engine.restore_backup(zip_path, "Map1", "Save1", str(save_dir)) is True
        # 备份目录不能被清掉
        assert (save_dir / "backup").is_dir()


class TestZipSlip:
    def test_rejects_path_traversal(self, tmp_path):
        _, engine, save_dir = make_env(tmp_path)
        engine.backup_save("Map1", "Save1", str(save_dir), "Map1/Save1")

        bad_zip = tmp_path / "evil.zip"
        with zipfile.ZipFile(bad_zip, "w") as zf:
            zf.writestr("../evil.txt", "boom")
            zf.writestr("a.txt", "evil-content")

        before = (save_dir / "a.txt").read_text(encoding="utf-8")
        assert engine.restore_backup(str(bad_zip), "Map1", "Save1", str(save_dir)) is False
        # 恶意文件未写出、原存档未被改动（校验发生在任何删除之前）
        assert not (tmp_path / "evil.txt").exists()
        assert not (saves_root(tmp_path) / "evil.txt").exists()
        assert (save_dir / "a.txt").read_text(encoding="utf-8") == before

    def test_rejects_absolute_paths(self, tmp_path):
        _, engine, save_dir = make_env(tmp_path)
        bad_zip = tmp_path / "abs.zip"
        with zipfile.ZipFile(bad_zip, "w") as zf:
            zf.writestr("/tmp/abs.txt", "x")
        assert engine.restore_backup(str(bad_zip), "Map1", "Save1", str(save_dir)) is False


def saves_root(tmp_path):
    return tmp_path / "Saves"
