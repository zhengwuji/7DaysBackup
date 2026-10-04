"""
入口文件 main.py
- 单实例锁（Windows 命名互斥量，无 PID 复用误判）
- 日志初始化（带轮转，避免无限增长）
- 模块组装与启动
- 命令行参数: --hidden（静默启动到托盘）
"""
import os
import sys
import logging
import logging.handlers
import ctypes
import threading
import time

# 添加项目根目录到 path
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from app.config import ConfigManager
from app.backup_engine import BackupEngine
from app.scheduler import BackupScheduler
from app.tray_app import TrayApp
from app.gui import BackupGUI
from app.startup import StartupManager

# ===================== 全局引用 =====================
# 模块实例（在 main 中赋值，供 shutdown 使用）
_app_config = None
_app_engine = None
_app_scheduler = None
_app_gui = None
_app_tray = None
_logger = None
_mutex_handle = None          # 单实例互斥量句柄，进程存活期间保持引用
_shutting_down = False

MUTEX_NAME = "Local\\7DaysBackup_SingleInstance"


# ===================== 日志 =====================

def setup_logging(config_dir: str):
    """配置日志：带轮转的文件输出 + stderr"""
    global _logger

    log_path = os.path.join(config_dir, "backup.log")
    os.makedirs(config_dir, exist_ok=True)

    _logger = logging.getLogger("BackupTool")
    _logger.setLevel(logging.INFO)

    # 文件 handler（1MB x 3 轮转）
    fh = logging.handlers.RotatingFileHandler(
        log_path, maxBytes=1024 * 1024, backupCount=3, encoding="utf-8"
    )
    fh.setLevel(logging.INFO)
    fh.setFormatter(logging.Formatter(
        "%(asctime)s [%(levelname)s] %(message)s", datefmt="%Y-%m-%d %H:%M:%S"
    ))
    _logger.addHandler(fh)

    # stderr handler (仅开发模式可见)
    sh = logging.StreamHandler(sys.stderr)
    sh.setLevel(logging.WARNING)
    sh.setFormatter(logging.Formatter("[%(levelname)s] %(message)s"))
    _logger.addHandler(sh)

    return _logger


# ===================== 单实例锁 =====================

def acquire_single_instance_lock() -> bool:
    """
    通过 Windows 命名互斥量保证单实例。
    互斥量随进程退出自动释放，不存在 PID 复用误判问题。
    """
    global _mutex_handle
    try:
        kernel32 = ctypes.windll.kernel32
        ERROR_ALREADY_EXISTS = 183

        handle = kernel32.CreateMutexW(None, False, MUTEX_NAME)
        if not handle:
            _logger and _logger.warning("创建实例互斥量失败，跳过单实例检测")
            return True  # 保守放行，避免因检测失败无法启动

        if kernel32.GetLastError() == ERROR_ALREADY_EXISTS:
            kernel32.CloseHandle(handle)
            _logger and _logger.error("已有实例正在运行")
            return False

        _mutex_handle = handle
        return True
    except Exception as e:
        _logger and _logger.warning("单实例检测失败: %s", e)
        return True


def release_single_instance_lock():
    """释放单实例互斥量"""
    global _mutex_handle
    if _mutex_handle:
        try:
            ctypes.windll.kernel32.CloseHandle(_mutex_handle)
        except Exception:
            pass
        _mutex_handle = None


# ===================== 退出处理 =====================

def shutdown():
    """优雅关闭所有模块（应在 tkinter 主线程执行，托盘退出经 schedule_ui_update 编组到此）"""
    global _shutting_down
    if _shutting_down:
        return
    _shutting_down = True

    if _logger:
        _logger.info("正在退出...")

    # 停止调度器
    if _app_scheduler:
        try:
            _app_scheduler.stop()
        except Exception:
            pass

    # 停止托盘
    if _app_tray:
        try:
            _app_tray.stop()
        except Exception:
            pass

    # 销毁 GUI
    if _app_gui and _app_gui._root:
        try:
            _app_gui._root.destroy()
        except Exception:
            pass

    # 释放锁
    release_single_instance_lock()

    if _logger:
        _logger.info("程序已退出")
        logging.shutdown()

    os._exit(0)


# ===================== 主流程 =====================

def _fmt_interval(seconds: int) -> str:
    """把秒数格式化为人类可读的间隔"""
    if seconds >= 3600 and seconds % 3600 == 0:
        return f"{seconds // 3600} 小时"
    if seconds >= 60 and seconds % 60 == 0:
        return f"{seconds // 60} 分钟"
    return f"{seconds} 秒"


def main():
    global _app_config, _app_engine, _app_scheduler, _app_gui, _app_tray

    # ---- 解析命令行 ----
    start_hidden = "--hidden" in sys.argv

    # ---- 初始化配置 ----
    _app_config = ConfigManager()

    # 日志
    setup_logging(_app_config.config_dir)
    _logger.info("=" * 50)
    _logger.info("7 Days Backup 启动 (PID: %d)", os.getpid())

    # 单实例锁
    if not acquire_single_instance_lock():
        _logger.error("检测到已有实例运行，退出")
        try:
            import tkinter.messagebox as mb
            mb.showerror("7 Days Backup", "程序已在运行中，请检查系统托盘。")
        except Exception:
            pass
        sys.exit(1)

    # ---- 模块初始化 ----
    _app_engine = BackupEngine(_app_config)
    # 清理上次异常退出残留的临时文件
    _app_engine.cleanup_tmp_files()
    _app_scheduler = BackupScheduler(_app_config, _app_engine)
    _app_gui = BackupGUI(_app_config, _app_engine, _app_scheduler, StartupManager)
    _app_tray = TrayApp("7DaysBackup - 存档备份中")

    # ---- 跨模块回调 ----

    # 调度器状态 -> 托盘 + GUI
    def on_scheduler_state(state: dict):
        status = state.get("status", "")
        if status:
            _app_tray.update_tooltip(f"7DaysBackup - {status}")
        if state.get("next_backup"):
            interval = state.get("interval")
            interval_text = f" | 间隔: {_fmt_interval(interval)}" if interval else ""
            _app_gui.update_next_backup(f"下次备份: {state['next_backup']}{interval_text}")

    _app_scheduler.set_state_callback(on_scheduler_state)

    # 备份引擎结果 -> GUI
    def on_backup_complete(status: dict):
        _app_gui.update_status(status.get("message", ""))
        _app_gui.refresh_list()

    _app_engine.set_status_callback(on_backup_complete)

    # 托盘菜单
    _app_tray.set_callback("show_window", lambda: _app_gui.schedule_ui_update(_app_gui.show))
    _app_tray.set_callback("backup_now", lambda: threading.Thread(
        target=_app_scheduler.run_once, daemon=True
    ).start())
    # 退出动作编组回 tkinter 主线程执行，避免跨线程销毁窗口
    _app_tray.set_callback("exit_app", lambda: _app_gui.schedule_ui_update(shutdown))

    # GUI 退出
    _app_gui.set_exit_callback(shutdown)

    # ---- 启动各模块 ----

    # 创建 GUI（隐藏状态）
    _app_gui.setup()

    # 开机自启：配置开关且未启用时自动创建
    if _app_config.get("auto_start") and not StartupManager.is_enabled():
        StartupManager.enable()

    # 是否显示窗口（先显示窗口，托盘等待不再阻塞界面出现）
    if not start_hidden:
        _app_gui.schedule_ui_update(_app_gui.show)
    else:
        _logger.info("静默启动到系统托盘")

    # 启动托盘图标（等待系统托盘就绪，失败则后台自动重试）
    tray_started = _app_tray.start()
    if not tray_started:
        _logger.warning("托盘图标暂时不可用（系统托盘未就绪），将后台重试")

    # 启动备份调度器
    _app_scheduler.start()

    # 延迟 3 秒后执行首次备份（走调度器，统一 busy 状态与提示）
    def delayed_first_backup():
        time.sleep(3)
        _app_scheduler.run_once()

    threading.Thread(target=delayed_first_backup, daemon=True).start()

    _logger.info("初始化完成，进入主循环")

    # ---- tkinter 主循环（阻塞）----
    try:
        _app_gui.run_mainloop()
    except KeyboardInterrupt:
        _logger.info("收到中断信号")
    finally:
        shutdown()


if __name__ == "__main__":
    main()
