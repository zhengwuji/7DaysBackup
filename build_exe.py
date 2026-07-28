"""
PyInstaller 打包脚本
将项目打包为单个 Windows EXE 文件（含图标和资源）
用法: python build_exe.py
"""
import os
import sys
import subprocess
import shutil

PROJECT_DIR = os.path.dirname(os.path.abspath(__file__))
MAIN_SCRIPT = os.path.join(PROJECT_DIR, "main.py")
OUTPUT_DIR = os.path.join(PROJECT_DIR, "dist")
BUILD_DIR = os.path.join(PROJECT_DIR, "build")
EXE_NAME = "7DaysBackup"
ICON_PATH = os.path.join(PROJECT_DIR, "resources", "icon.ico")
RESOURCES_DIR = os.path.join(PROJECT_DIR, "resources")


def run_pyinstaller():
    """执行 PyInstaller 打包"""
    cmd = [
        sys.executable, "-m", "PyInstaller",
        "--name", EXE_NAME,
        "--noconsole",
        "--onefile",
        "--clean",
        f"--distpath={OUTPUT_DIR}",
        f"--workpath={BUILD_DIR}",
        # 图标
        f"--icon={ICON_PATH}",
        # 资源文件
        "--add-data", f"{RESOURCES_DIR}{os.pathsep}resources",
        # pystray 需要的隐式导入
        "--hidden-import", "pystray._win32",
        "--hidden-import", "pystray._util.win32",
        "--hidden-import", "PIL._tkinter_finder",
        # tkinter
        "--hidden-import", "tkinter",
        "--hidden-import", "tkinter.ttk",
        "--hidden-import", "tkinter.messagebox",
        # 排除不必要模块
        "--exclude-module", "matplotlib",
        "--exclude-module", "numpy",
        "--exclude-module", "pandas",
        "--exclude-module", "scipy",
        "--exclude-module", "IPython",
        MAIN_SCRIPT,
    ]

    print("=" * 50)
    print("执行 PyInstaller 打包...")
    print("=" * 50)

    result = subprocess.run(cmd, cwd=PROJECT_DIR)

    if result.returncode == 0:
        exe_path = os.path.join(OUTPUT_DIR, f"{EXE_NAME}.exe")
        if os.path.exists(exe_path):
            size_mb = os.path.getsize(exe_path) / (1024 * 1024)
            print(f"\n{'=' * 50}")
            print(f"  打包成功!")
            print(f"  输出: {exe_path}")
            print(f"  大小: {size_mb:.1f} MB")
            print(f"{'=' * 50}")
            print(f"\n使用方法:")
            print(f"  双击运行        -> 显示窗口")
            print(f"  {EXE_NAME}.exe --hidden -> 静默到托盘")
        else:
            print("\n打包完成但未找到 exe 文件")
    else:
        print(f"\n打包失败，错误码: {result.returncode}")
        sys.exit(result.returncode)


def clean():
    """清理构建文件"""
    for path in [BUILD_DIR, os.path.join(PROJECT_DIR, f"{EXE_NAME}.spec")]:
        if os.path.isdir(path):
            shutil.rmtree(path, ignore_errors=True)
        elif os.path.isfile(path):
            os.remove(path)
    print("清理完成")


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description="PyInstaller 打包")
    parser.add_argument("--clean-only", action="store_true", help="仅清理构建文件")
    args = parser.parse_args()

    if args.clean_only:
        clean()
    else:
        run_pyinstaller()
