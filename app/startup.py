"""
开机自启管理模块
通过注册表 HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run 键实现
（无需 PowerShell/COM，读写即时生效），并兼容清理旧版的启动文件夹 .lnk 快捷方式
"""
import os
import sys
import logging

logger = logging.getLogger("BackupTool")

try:
    import winreg
    WINREG_AVAILABLE = True
except ImportError:
    WINREG_AVAILABLE = False

RUN_KEY_PATH = r"Software\Microsoft\Windows\CurrentVersion\Run"
RUN_VALUE_NAME = "7DaysBackup"
SHORTCUT_NAME = "7DaysBackup.lnk"   # 旧版创建的快捷方式，启用注册表方式后清理


class StartupManager:
    """Windows 开机自启管理"""

    # ---------- 旧版 .lnk 兼容 ----------

    @staticmethod
    def get_startup_dir():
        """获取 Windows 启动文件夹路径"""
        return os.path.join(
            os.getenv("APPDATA") or os.path.expanduser("~"),
            r"Microsoft\Windows\Start Menu\Programs\Startup"
        )

    @classmethod
    def get_shortcut_path(cls):
        return os.path.join(cls.get_startup_dir(), SHORTCUT_NAME)

    @classmethod
    def _remove_legacy_shortcut(cls):
        """删除旧版启动文件夹快捷方式（若存在）"""
        shortcut_path = cls.get_shortcut_path()
        try:
            if os.path.exists(shortcut_path):
                os.remove(shortcut_path)
                logger.info("已清理旧版启动快捷方式: %s", shortcut_path)
        except OSError as e:
            logger.warning("清理旧版启动快捷方式失败: %s", e)

    # ---------- 启动命令 ----------

    @staticmethod
    def _get_command() -> str:
        """生成启动命令：exe 路径加引号，附加 --hidden 静默启动"""
        if getattr(sys, "frozen", False):
            return f'"{sys.executable}" --hidden'
        # 开发模式：优先 pythonw.exe 启动 main.py
        exe = sys.executable.replace("python.exe", "pythonw.exe")
        if not os.path.exists(exe):
            exe = sys.executable
        main_py = os.path.join(
            os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "main.py"
        )
        return f'"{exe}" "{main_py}" --hidden'

    # ---------- 开关 ----------

    @classmethod
    def is_enabled(cls) -> bool:
        """检查是否已启用开机自启（注册表键值或旧版 .lnk 存在均视为启用）"""
        if WINREG_AVAILABLE:
            try:
                with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY_PATH) as key:
                    winreg.QueryValueEx(key, RUN_VALUE_NAME)
                return True
            except FileNotFoundError:
                pass
            except OSError as e:
                logger.warning("读取自启注册表失败: %s", e)
        return os.path.exists(cls.get_shortcut_path())

    @classmethod
    def enable(cls, exe_path=None):
        """启用开机自启（exe_path 参数保留兼容旧签名，现忽略）"""
        if not WINREG_AVAILABLE:
            return False, "仅支持 Windows"
        try:
            with winreg.OpenKey(
                winreg.HKEY_CURRENT_USER, RUN_KEY_PATH, 0, winreg.KEY_SET_VALUE
            ) as key:
                winreg.SetValueEx(
                    key, RUN_VALUE_NAME, 0, winreg.REG_SZ, cls._get_command()
                )
            cls._remove_legacy_shortcut()
            logger.info("开机自启已启用: HKCU\\%s\\%s", RUN_KEY_PATH, RUN_VALUE_NAME)
            return True, "开机自启已启用"
        except OSError as e:
            logger.error("启用开机自启失败: %s", e)
            return False, f"启用失败: {e}"

    @classmethod
    def disable(cls):
        """禁用开机自启"""
        removed = False
        if WINREG_AVAILABLE:
            try:
                with winreg.OpenKey(
                    winreg.HKEY_CURRENT_USER, RUN_KEY_PATH, 0, winreg.KEY_SET_VALUE
                ) as key:
                    winreg.DeleteValue(key, RUN_VALUE_NAME)
                removed = True
            except FileNotFoundError:
                pass
            except OSError as e:
                logger.error("禁用开机自启失败: %s", e)
                return False, f"禁用失败: {e}"
        cls._remove_legacy_shortcut()
        if removed or not WINREG_AVAILABLE:
            return True, "开机自启已禁用"
        return True, "开机自启未启用"

    @classmethod
    def toggle(cls, enable: bool, exe_path=None):
        """切换开机自启状态"""
        if enable:
            return cls.enable(exe_path)
        else:
            return cls.disable()
