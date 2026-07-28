"""
定时调度器模块
基于 threading.Timer 的轻量级定时器
支持启动、停止、暂停、恢复、间隔变更
"""
import threading
import logging
from datetime import datetime, timedelta

logger = logging.getLogger("BackupTool")


class BackupScheduler:
    """轻量备份调度器"""

    def __init__(self, config_manager, backup_engine):
        self._config = config_manager
        self._engine = backup_engine
        self._timer = None
        self._paused = False
        self._running = False
        self._busy = False          # 防止并发备份
        self._lock = threading.Lock()
        self._state_callback = None  # fn(state_dict)

    def set_state_callback(self, callback):
        self._state_callback = callback

    def _notify_state(self, **kwargs):
        if self._state_callback:
            try:
                self._state_callback(kwargs)
            except Exception:
                pass

    @property
    def is_running(self):
        with self._lock:
            return self._running

    @property
    def is_paused(self):
        with self._lock:
            return self._paused

    def start(self):
        """启动调度器"""
        with self._lock:
            if self._running:
                return
            self._running = True
            self._paused = False
        logger.info("调度器已启动，间隔 %d 秒", self._config.get("backup_interval_seconds", 60))
        self._notify_state(running=True, paused=False)
        self._schedule_next()

    def stop(self):
        """停止调度器"""
        with self._lock:
            self._running = False
            self._paused = False
            if self._timer:
                self._timer.cancel()
                self._timer = None
        logger.info("调度器已停止")
        self._notify_state(running=False, paused=False)

    def pause(self):
        """暂停调度（跳过下一次触发）"""
        with self._lock:
            if not self._running or self._paused:
                return
            self._paused = True
            if self._timer:
                self._timer.cancel()
                self._timer = None
        logger.info("调度器已暂停")
        self._notify_state(running=True, paused=True)

    def resume(self):
        """恢复调度"""
        with self._lock:
            if not self._running or not self._paused:
                return
            self._paused = False
        logger.info("调度器已恢复")
        self._notify_state(running=True, paused=False)
        self._schedule_next()

    def update_interval(self, new_interval_seconds: int):
        """动态更新间隔（>0 才有效）"""
        if new_interval_seconds <= 0:
            return
        self._config.set("backup_interval_seconds", new_interval_seconds)
        logger.info("备份间隔已更新为 %d 秒", new_interval_seconds)
        # 重新调度
        with self._lock:
            if self._timer:
                self._timer.cancel()
                self._timer = None
        if self._running and not self._paused:
            self._schedule_next()

    def _schedule_next(self):
        """安排下一次触发"""
        with self._lock:
            if not self._running or self._paused:
                return
        interval = self._config.get("backup_interval_seconds", 60)
        self._timer = threading.Timer(interval, self._on_tick)
        self._timer.daemon = True
        self._timer.start()

        next_time = (datetime.now() + timedelta(seconds=interval)).strftime("%H:%M:%S")
        self._notify_state(next_backup=next_time, interval=interval)

    def _on_tick(self):
        """定时器触发"""
        with self._lock:
            if self._busy:
                logger.info("上次备份未完成，跳过本次触发")
                self._schedule_next()
                return
            self._busy = True

        logger.info("定时器触发备份")
        self._notify_state(status="备份中...")
        try:
            self._engine.run_backup()
        except Exception as e:
            logger.error("备份执行异常: %s", e)
        finally:
            with self._lock:
                self._busy = False
        self._notify_state(status="空闲")
        # 安排下一次
        self._schedule_next()

    def run_once(self):
        """立即执行一次备份（手动触发，不改变定时器状态）"""
        with self._lock:
            if self._busy:
                logger.info("备份正在进行中，手动触发被忽略")
                return {"message": "备份正在进行中，请稍后重试"}
            self._busy = True

        logger.info("手动触发备份")
        self._notify_state(status="手动备份中...")
        try:
            status = self._engine.run_backup()
        except Exception as e:
            logger.error("手动备份异常: %s", e)
            status = {"message": f"备份失败: {e}"}
        finally:
            with self._lock:
                self._busy = False
        self._notify_state(status="空闲")
        return status
