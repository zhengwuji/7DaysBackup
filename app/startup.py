"""
开机自启管理模块
在 Windows 启动文件夹中创建/删除快捷方式
使用 PowerShell COM 对象创建 .lnk 文件
"""
import os
import sys
import subprocess
import logging

logger = logging.getLogger("BackupTool")


class StartupManager:
    """Windows 开机自启管理"""

    SHORTCUT_NAME = "7DaysBackup.lnk"

    @staticmethod
    def get_startup_dir():
        """获取 Windows 启动文件夹路径"""
        return os.path.join(
            os.getenv("APPDATA"),
            r"Microsoft\Windows\Start Menu\Programs\Startup"
        )

    @classmethod
    def get_shortcut_path(cls):
        return os.path.join(cls.get_startup_dir(), cls.SHORTCUT_NAME)

    @classmethod
    def is_enabled(cls):
        """检查是否已启用开机自启"""
        return os.path.exists(cls.get_shortcut_path())

    @classmethod
    def enable(cls, exe_path=None):
        """
        启用开机自启
        - 在启动文件夹创建快捷方式
        - exe_path: 程序路径，默认为当前运行的可执行文件
        """
        if exe_path is None:
            # 如果是 PyInstaller 打包的 exe
            if getattr(sys, 'frozen', False):
                exe_path = sys.executable
            else:
                # 开发模式，使用 pythonw.exe 启动 main.py
                exe_path = sys.executable.replace("python.exe", "pythonw.exe")
                # pythonw.exe 可能不存在，回退到 python.exe
                if not os.path.exists(exe_path):
                    exe_path = sys.executable

        shortcut_path = cls.get_shortcut_path()
        work_dir = os.path.dirname(exe_path)

        try:
            # 先确保启动目录存在
            os.makedirs(cls.get_startup_dir(), exist_ok=True)

            # 删除已存在的快捷方式
            if os.path.exists(shortcut_path):
                os.remove(shortcut_path)

            # 使用 PowerShell 创建快捷方式
            ps_script = f'''
$WshShell = New-Object -ComObject WScript.Shell
$Shortcut = $WshShell.CreateShortcut("{shortcut_path}")
$Shortcut.TargetPath = "{exe_path}"
$Shortcut.Arguments = "--hidden"
$Shortcut.WorkingDirectory = "{work_dir}"
$Shortcut.WindowStyle = 7
$Shortcut.Save()
'''
            result = subprocess.run(
                ["powershell", "-NoProfile", "-Command", ps_script],
                capture_output=True, text=True, timeout=10
            )

            if result.returncode == 0:
                logger.info("开机自启已启用: %s", shortcut_path)
                return True, "开机自启已启用"
            else:
                logger.error("创建快捷方式失败: %s", result.stderr)
                return False, f"创建快捷方式失败: {result.stderr}"

        except subprocess.TimeoutExpired:
            return False, "创建快捷方式超时"
        except Exception as e:
            logger.error("启用开机自启失败: %s", e)
            return False, f"启用失败: {e}"

    @classmethod
    def disable(cls):
        """禁用开机自启"""
        shortcut_path = cls.get_shortcut_path()
        try:
            if os.path.exists(shortcut_path):
                os.remove(shortcut_path)
                logger.info("开机自启已禁用")
                return True, "开机自启已禁用"
            return True, "开机自启未启用"
        except Exception as e:
            logger.error("禁用开机自启失败: %s", e)
            return False, f"禁用失败: {e}"

    @classmethod
    def toggle(cls, enable: bool, exe_path=None):
        """切换开机自启状态"""
        if enable:
            return cls.enable(exe_path)
        else:
            return cls.disable()
