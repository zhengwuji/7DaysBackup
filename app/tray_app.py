"""
系统托盘模块
提供托盘图标、右键菜单、气泡提示
使用 pystray + Pillow 实现
开机自启时系统托盘（Explorer）可能未就绪，需等待并重试
"""
import os
import sys
import time
import ctypes
import threading
import logging
from PIL import Image, ImageDraw

logger = logging.getLogger("BackupTool")

# 尝试导入 pystray
try:
    import pystray
    PYSTRAY_AVAILABLE = True
except ImportError:
    PYSTRAY_AVAILABLE = False
    logger.warning("pystray 未安装，托盘功能不可用")


def _system_tray_ready() -> bool:
    """检测 Windows 系统托盘（Shell_TrayWnd）是否就绪"""
    try:
        user32 = ctypes.windll.user32
        return bool(user32.FindWindowW("Shell_TrayWnd", None))
    except Exception:
        return False


def _wait_for_system_tray(timeout: int = 60) -> bool:
    """等待系统托盘就绪（开机自启时 Explorer 可能还没初始化完成）"""
    start = time.time()
    while time.time() - start < timeout:
        if _system_tray_ready():
            return True
        time.sleep(1)
    return False


def _get_resource_path(filename):
    """获取资源文件路径（兼容 PyInstaller 打包和开发模式）"""
    if getattr(sys, 'frozen', False):
        # PyInstaller 打包后
        base = sys._MEIPASS
    else:
        # 开发模式
        base = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    return os.path.join(base, "resources", filename)


def _create_icon_image(size=64):
    """生成默认图标（后备方案）"""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    # 绿色圆角矩形背景
    draw.rounded_rectangle(
        [size // 8, size // 8, size - size // 8, size - size // 8],
        radius=size // 6,
        fill=(46, 125, 50, 255),
    )

    # 内部白色圆形
    im = size // 4
    draw.ellipse([im, im, size - im, size - im], fill=(255, 255, 255, 255))

    # 向上箭头（绿色）
    cx, cy = size // 2, size // 2
    a = size // 6
    draw.polygon([(cx, cy - a - 2), (cx - a - 1, cy + 2), (cx + a + 1, cy + 2)],
                 fill=(46, 125, 50, 255))

    return img


def _load_icon_image():
    """加载托盘图标（优先 icon.ico，回退到 icon.png 或生成图标）"""
    for fname in ("icon.ico", "icon.png"):
        try:
            path = _get_resource_path(fname)
            if os.path.exists(path):
                return Image.open(path)
        except Exception as e:
            logger.debug("加载 %s 失败: %s", fname, e)

    # 回退到生成图标
    return _create_icon_image()


class TrayApp:
    """系统托盘应用"""

    def __init__(self, title="7 Days Backup"):
        self._title = title
        self._icon = None
        self._callbacks = {
            "show_window": None,
            "backup_now": None,
            "exit_app": None,
        }
        self._running = False

    # ========== 回调注册 ==========

    def set_callback(self, name, fn):
        """注册回调: show_window, backup_now, exit_app"""
        self._callbacks[name] = fn

    # ========== 托盘菜单 ==========

    def _build_menu(self):
        """构建右键菜单"""
        menu_items = []

        # 显示设置
        menu_items.append(
            pystray.MenuItem("显示设置", self._on_show, default=True)
        )
        menu_items.append(pystray.Menu.SEPARATOR)

        # 立即备份
        menu_items.append(
            pystray.MenuItem("立即备份", self._on_backup_now)
        )
        menu_items.append(pystray.Menu.SEPARATOR)

        # 退出
        menu_items.append(
            pystray.MenuItem("退出", self._on_exit)
        )

        return pystray.Menu(*menu_items)

    # ========== 菜单事件 ==========

    def _on_show(self, icon, item):
        if self._callbacks["show_window"]:
            self._callbacks["show_window"]()

    def _on_backup_now(self, icon, item):
        if self._callbacks["backup_now"]:
            self._callbacks["backup_now"]()

    def _on_exit(self, icon, item):
        if self._callbacks["exit_app"]:
            self._callbacks["exit_app"]()
        else:
            self.stop()

    # ========== 生命周期 ==========

    def start(self):
        """启动托盘图标（等待系统托盘就绪，失败则后台重试）"""
        if not PYSTRAY_AVAILABLE:
            logger.error("pystray 未安装，无法创建托盘图标")
            return False

        if self._running:
            return True

        # 等待系统托盘就绪（开机自启时 Explorer 可能还没初始化）
        if not _wait_for_system_tray(timeout=15):
            logger.warning("等待系统托盘超时，将在后台持续重试")
            threading.Thread(target=self._background_retry, daemon=True).start()
            return False

        return self._do_start()

    def _do_start(self):
        """实际创建托盘图标"""
        if self._running:
            return True

        try:
            image = _load_icon_image()
            menu = self._build_menu()

            self._icon = pystray.Icon(
                name="7DaysBackup",
                icon=image,
                title=self._title,
                menu=menu,
            )

            logger.info("托盘图标已创建")
            self._running = True

            # 在守护线程中运行（非阻塞）
            thread = threading.Thread(target=self._icon.run, daemon=True)
            thread.start()
            return True

        except Exception as e:
            logger.error("创建托盘图标失败: %s", e)
            return False

    def _background_retry(self):
        """后台持续重试创建托盘（每 5 秒一次，直到成功）"""
        attempt = 0
        while not self._running:
            attempt += 1
            time.sleep(5)
            if _system_tray_ready():
                if self._do_start():
                    logger.info("托盘图标重试成功（第 %d 次）", attempt)
                    return
            if attempt > 120:  # 最多重试约 10 分钟
                logger.error("托盘图标重试失败，已放弃")
                return

    def stop(self):
        """停止托盘图标"""
        self._running = False
        if self._icon:
            try:
                self._icon.stop()
                self._icon = None
                logger.info("托盘图标已停止")
            except Exception as e:
                logger.warning("停止托盘图标异常: %s", e)

    def show_notification(self, title: str, message: str):
        """显示气泡通知"""
        if self._icon and self._running:
            try:
                self._icon.notify(message, title)
            except Exception as e:
                logger.debug("气泡通知失败: %s", e)

    def update_tooltip(self, text: str):
        """更新悬停提示文本"""
        if self._icon and self._running:
            try:
                self._icon.title = text
            except Exception:
                pass
