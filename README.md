# 7DaysBackup — 七日杀存档自动备份工具

> 你负责在废土里浪，它负责给你兜底。

在《七日杀》里，最让人破防的不是被僵尸围殴致死，而是花了一整个周末建的末日堡垒，在一次闪退之后全部归零。7DaysBackup 就是为这个场景而生的——一个驻留在系统托盘的静默备份程序，你只管玩，它只管备份。

## 功能

| 功能 | 说明 |
|------|------|
| 热备份 | 边玩边备份，不需要退出游戏 |
| 定时快照 | 默认每 5 分钟自动备份，间隔可调 |
| 变更检测 | 只备份发生变化的活跃存档，其他跳过 |
| 版本轮转 | 保留最近 N 个版本（默认 10），自动清理旧备份 |
| zip 压缩 | Python 内置压缩，无需安装 7-Zip / WinRAR |
| 一键恢复 | 选中备份版本，确认后自动解压覆盖，恢复前自动创建安全快照 |
| 系统托盘 | 静默运行，不弹窗打扰，双击图标打开设置面板 |
| 开机自启 | 默认开启，可在设置中关闭 |
| 存档勾选 | 左侧列表勾选需要监控的存档，不勾的不备份 |
| 单文件 | 一个 18MB 的 exe，拷到 U 盘都能用 |

## 界面一览

```
┌──────────────┬───────────────────┐
│  存档列表     │  备份历史          │
│  ☑ Navezgane │ 2026-07-28 17:03  │
│    ☑ 30test  │ 2026-07-28 16:55  │
│    ☐ 30test2 │                   │
│              │ [恢复选中备份]     │
├──────────────┴───────────────────┤
│  设置  间隔[5]分  版本[10]  开机自启 │
│  [立即备份] [隐藏窗口]            │
└──────────────────────────────────┘
```

## 技术栈

- **Python 3.12** — 纯标准库驱动
- **tkinter** — 内置 GUI，零额外依赖
- **pystray + Pillow** — 系统托盘
- **zipfile** — 内置压缩，不依赖外部压缩工具
- **PyInstaller** — 打包为单文件 exe

## 快速开始

### 从源码运行

```bash
pip install -r requirements.txt
python main.py              # 显示窗口
python main.py --hidden     # 静默到托盘
```

### 打包

```bash
python build_exe.py
# 输出: dist/7DaysBackup.exe
```

### 直接使用

下载 [Releases](https://github.com/Lumore/7DaysBackup/releases) 中的 `7DaysBackup.exe`，双击运行即可。

## 配置文件

首次运行自动生成于 `%APPDATA%/7DaysBackup/config.json`：

```json
{
  "backup_interval_seconds": 300,
  "max_backup_versions": 10,
  "monitored_saves": ["Navezgane/30test", ...],
  "auto_start": true,
  "minimize_to_tray": true
}
```

## License

MIT © Lumore
