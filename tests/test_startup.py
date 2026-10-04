"""StartupManager 基础回归（仅只读操作，不改动真实注册表/启动文件夹）"""
from app.startup import StartupManager


class TestStartupBasics:
    def test_is_enabled_returns_bool(self):
        # 回归：SHORTCUT_NAME 曾被错误地按类属性引用，GUI 构建时必崩
        assert isinstance(StartupManager.is_enabled(), bool)

    def test_shortcut_path_composition(self):
        path = StartupManager.get_shortcut_path()
        assert path.endswith("7DaysBackup.lnk")
        assert "Startup" in path

    def test_get_command_quotes_paths_and_hidden(self):
        cmd = StartupManager._get_command()
        assert cmd.startswith('"')
        assert '" --hidden' in cmd
