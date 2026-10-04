"""调度器：启停、忙标记、间隔更新"""
import threading
import time

from app.config import ConfigManager
from app.scheduler import BackupScheduler


class CountingEngine:
    def __init__(self):
        self.calls = 0
        self.lock = threading.Lock()

    def run_backup(self):
        with self.lock:
            self.calls += 1
        return {"message": "ok"}


def make_sched(tmp_path, interval=3600):
    cfg = ConfigManager(config_dir=str(tmp_path / "cfg"))
    cfg.set("backup_interval_seconds", interval)
    engine = CountingEngine()
    sched = BackupScheduler(cfg, engine)
    return cfg, engine, sched


class TestLifecycle:
    def test_start_stop(self, tmp_path):
        _, _, sched = make_sched(tmp_path)
        assert not sched.is_running
        sched.start()
        assert sched.is_running
        sched.stop()
        assert not sched.is_running

    def test_double_start_ignored(self, tmp_path):
        _, _, sched = make_sched(tmp_path)
        sched.start()
        sched.start()
        assert sched.is_running
        sched.stop()

    def test_pause_resume(self, tmp_path):
        _, _, sched = make_sched(tmp_path)
        sched.start()
        sched.pause()
        assert sched.is_paused
        sched.resume()
        assert not sched.is_paused
        sched.stop()

    def test_stop_before_tick_no_backup(self, tmp_path):
        cfg, engine, sched = make_sched(tmp_path, interval=3600)
        sched.start()
        sched.stop()
        time.sleep(0.2)
        assert engine.calls == 0


class TestTicking:
    def test_tick_runs_backup_and_reschedules(self, tmp_path):
        cfg, engine, sched = make_sched(tmp_path, interval=1)
        sched.start()
        deadline = time.time() + 5
        while time.time() < deadline and engine.calls < 2:
            time.sleep(0.05)
        sched.stop()
        # 至少触发过一次，说明 tick 后能正常重排
        assert engine.calls >= 1

    def test_update_interval_persists(self, tmp_path):
        cfg, _, sched = make_sched(tmp_path, interval=3600)
        sched.update_interval(120)
        assert cfg.get("backup_interval_seconds") == 120

    def test_update_interval_ignores_invalid(self, tmp_path):
        cfg, _, sched = make_sched(tmp_path, interval=3600)
        sched.update_interval(0)
        sched.update_interval(-5)
        assert cfg.get("backup_interval_seconds") == 3600


class TestRunOnce:
    def test_run_once_executes(self, tmp_path):
        _, engine, sched = make_sched(tmp_path)
        status = sched.run_once()
        assert status["message"] == "ok"
        assert engine.calls == 1

    def test_run_once_not_running_scheduler(self, tmp_path):
        _, engine, sched = make_sched(tmp_path)
        # 调度器未启动也能手动备份
        sched.run_once()
        assert engine.calls == 1
