"""
入口文件 main.py
- 单实例进程锁
- 日志初始化
- 模块组装与启动
- 命令行参数: --hidden（静默启动到托盘）
"""
import os
import sys
import logging
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


# ===================== 日志 =====================

def setup_logging(config_dir: str):
    """配置日志：同时输出到文件和 stderr"""
    global _logger

    log_path = os.path.join(config_dir, "backup.log")
    os.makedirs(config_dir, exist_ok=True)

    _logger = logging.getLogger("BackupTool")
    _logger.setLevel(logging.INFO)

    # 文件 handler
    fh = logging.FileHandler(log_path, encoding="utf-8")
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

def acquire_single_instance_lock(lock_dir: str) -> bool:
    """尝试获取单实例锁"""
    lock_path = os.path.join(lock_dir, "instance.lock")
    os.makedirs(lock_dir, exist_ok=True)

    try:
        if os.path.exists(lock_path):
            try:
                with open(lock_path, "r") as f:
                    old_pid = int(f.read().strip())
                # 检查进程是否还活着
                import ctypes
                kernel32 = ctypes.windll.kernel32
                handle = kernel32.OpenProcess(0x0400, False, old_pid)
                if handle:
                    kernel32.CloseHandle(handle)
                    _logger and _logger.error("已有实例正在运行 (PID: %d)", old_pid)
                    return False
            except (ValueError, FileNotFoundError):
                pass

        with open(lock_path, "w") as f:
            f.write(str(os.getpid()))
        return True
    except Exception as e:
        _logger and _logger.warning("创建实例锁失败: %s", e)
        return True


def release_single_instance_lock(lock_dir: str):
    """释放单实例锁"""
    lock_path = os.path.join(lock_dir, "instance.lock")
    try:
        if os.path.exists(lock_path):
            os.remove(lock_path)
    except Exception:
        pass


# ===================== 退出处理 =====================

def shutdown():
    """优雅关闭所有模块"""
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
    if _app_config:
        release_single_instance_lock(_app_config.config_dir)

    if _logger:
        _logger.info("程序已退出")

    os._exit(0)


# ===================== 主流程 =====================

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
    if not acquire_single_instance_lock(_app_config.config_dir):
        _logger.error("检测到已有实例运行，退出")
        try:
            import tkinter.messagebox as mb
            mb.showerror("7 Days Backup", "程序已在运行中，请检查系统托盘。")
        except Exception:
            pass
        sys.exit(1)

    # ---- 模块初始化 ----
    _app_engine = BackupEngine(_app_config)
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
            _app_gui.update_next_backup(
                f"下次备份: {state['next_backup']} | 间隔: {state.get('interval', '--')} 秒"
            )

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
    _app_tray.set_callback("exit_app", shutdown)

    # GUI 退出
    _app_gui.set_exit_callback(shutdown)

    # ---- 启动各模块 ----

    # 创建 GUI（隐藏状态）
    _app_gui.setup()

    # 开机自启：配置开关且快捷方式不存在时自动创建
    if _app_config.get("auto_start") and not StartupManager.is_enabled():
        StartupManager.enable()

    # 启动托盘图标（等待系统托盘就绪，失败则后台自动重试）
    tray_started = _app_tray.start()
    if not tray_started:
        _logger.warning("托盘图标暂时不可用（系统托盘未就绪），将后台重试")
        # 非静默模式下先显示窗口，避免用户看不到程序
        if not start_hidden:
            _app_gui.show()

    # 是否显示窗口
    if not start_hidden:
        _app_gui.schedule_ui_update(_app_gui.show)
    else:
        _logger.info("静默启动到系统托盘")

    # 启动备份调度器
    _app_scheduler.start()

    # 延迟 3 秒后执行首次备份
    def delayed_first_backup():
        time.sleep(3)
        _app_engine.run_backup()

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
