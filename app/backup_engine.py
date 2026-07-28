"""
备份引擎模块
- 扫描存档目录结构
- 检测存档变化（基于目录修改时间）
- 执行 zip 压缩备份
- 清理过期备份版本
"""
import os
import zipfile
import shutil
import logging
import threading
from datetime import datetime

logger = logging.getLogger("BackupTool")


class BackupEngine:
    """备份引擎，负责扫描、压缩、清理"""

    def __init__(self, config_manager):
        self._config = config_manager
        self._lock = threading.Lock()
        self._status_callback = None      # 状态变更回调: fn(status_dict)

    def set_status_callback(self, callback):
        """设置状态回调，用于通知 GUI 更新"""
        self._status_callback = callback

    # ========== 扫描 ==========

    def scan_saves(self):
        """
        扫描存档目录，返回所有存档列表
        返回: [(map_name, save_name, full_path, relative_key), ...]
              relative_key 格式: "MapName/SaveName"
        目录结构: Saves/{MapName}/{SaveName}/
        """
        saves = []
        root = self._config.save_path

        if not os.path.isdir(root):
            logger.warning("存档目录不存在: %s", root)
            return saves

        for map_name in os.listdir(root):
            map_path = os.path.join(root, map_name)
            if not os.path.isdir(map_path):
                continue
            for save_name in os.listdir(map_path):
                save_path = os.path.join(map_path, save_name)
                if not os.path.isdir(save_path):
                    continue
                relative_key = f"{map_name}/{save_name}"
                saves.append((map_name, save_name, save_path, relative_key))

        return saves

    # ========== 变更检测 ==========

    def get_save_last_modified(self, save_path: str):
        """
        获取存档目录最新文件修改时间（递归遍历，排除备份目录）
        返回 datetime 对象
        """
        max_mtime = 0
        try:
            for root, dirs, files in os.walk(save_path):
                # 排除备份目录，避免备份文件干扰变更检测
                if "backup" in dirs:
                    dirs.remove("backup")
                for name in files:
                    full = os.path.join(root, name)
                    try:
                        mtime = os.path.getmtime(full)
                        if mtime > max_mtime:
                            max_mtime = mtime
                    except OSError:
                        pass
        except Exception as e:
            logger.warning("遍历存档目录失败 %s: %s", save_path, e)

        if max_mtime == 0:
            return None
        return datetime.fromtimestamp(max_mtime)

    def check_saves_changed(self):
        """
        检查所有被监控的存档，返回发生变化的存档列表
        返回: [(map_name, save_name, save_path, relative_key), ...]
        """
        changed = []
        all_saves = self.scan_saves()
        monitored = set(self._config.get_monitored_saves())

        for map_name, save_name, save_path, relative_key in all_saves:
            # 只检查被勾选监控的存档
            if relative_key not in monitored:
                continue

            last_backup_str = self._config.get_last_backup_time(relative_key)
            if last_backup_str is None:
                # 从未备份过，需要备份
                changed.append((map_name, save_name, save_path, relative_key))
                continue

            # 检查目录修改时间是否晚于上次备份
            last_mtime = self.get_save_last_modified(save_path)
            if last_mtime is None:
                continue

            try:
                last_backup_time = datetime.fromisoformat(last_backup_str)
                if last_mtime > last_backup_time:
                    changed.append((map_name, save_name, save_path, relative_key))
            except ValueError:
                changed.append((map_name, save_name, save_path, relative_key))

        return changed

    # ========== 备份 ==========

    def backup_save(self, map_name: str, save_name: str, save_path: str, relative_key: str) -> bool:
        """
        对单个存档执行备份
        - 压缩存档目录为 zip（排除 backup 目录避免递归）
        - 先写入临时文件，再移动到 {save_path}/backup/ 下
        - 命名: {MapName}_{SaveName}_{yyyyMMdd_HHmmss}.zip
        返回是否成功
        """
        # 初始化临时路径（在 try 外部，确保 finally 可访问）
        tmp_path = ""
        try:
            # 确保 backup 目录存在
            backup_dir = os.path.join(save_path, "backup")
            os.makedirs(backup_dir, exist_ok=True)

            # 生成备份文件名（临时文件放在 map 目录下，避免 zip 自包含）
            timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
            zip_filename = f"{map_name}_{save_name}_{timestamp}.zip"
            map_dir = os.path.dirname(save_path)
            tmp_path = os.path.join(map_dir, f"_{zip_filename}.tmp")
            final_path = os.path.join(backup_dir, zip_filename)

            logger.info("开始备份: %s/%s -> %s", map_name, save_name, final_path)

            # 使用 zipfile 逐文件压缩，排除 backup 目录
            file_count = 0
            total_size = 0
            with zipfile.ZipFile(tmp_path, "w", zipfile.ZIP_DEFLATED) as zf:
                for root, dirs, files in os.walk(save_path):
                    # 排除 backup 目录
                    if "backup" in dirs:
                        dirs.remove("backup")

                    for file in files:
                        file_path = os.path.join(root, file)
                        arcname = os.path.relpath(file_path, save_path)
                        try:
                            zf.write(file_path, arcname)
                            file_count += 1
                            total_size += os.path.getsize(file_path)
                        except OSError as e:
                            logger.debug("跳过文件 %s: %s", file_path, e)

            # 移动到 backup 目录
            if os.path.exists(final_path):
                os.remove(final_path)
            os.rename(tmp_path, final_path)

            zip_size_mb = os.path.getsize(final_path) / (1024 * 1024)
            source_size_mb = total_size / (1024 * 1024)
            logger.info(
                "备份完成: %s (%d 个文件, 原始 %.1f MB, 压缩后 %.1f MB)",
                zip_filename, file_count, source_size_mb, zip_size_mb
            )

            # 记录备份时间
            now_iso = datetime.now().isoformat()
            self._config.set_last_backup_time(relative_key, now_iso)

            return True

        except Exception as e:
            logger.error("备份失败 %s/%s: %s", map_name, save_name, e, exc_info=True)
            # 清理临时文件
            try:
                if os.path.exists(tmp_path):
                    os.remove(tmp_path)
            except Exception:
                pass
            return False

    # ========== 清理 ==========

    def cleanup_old_backups(self, save_path: str) -> int:
        """
        清理超过最大版本数的旧备份文件
        删除最旧的备份，保留最新的 N 个
        返回删除的文件数
        """
        max_versions = self._config.get("max_backup_versions", 10)
        backup_dir = os.path.join(save_path, "backup")

        if not os.path.isdir(backup_dir):
            return 0

        # 列出所有 .zip 备份文件，按修改时间排序
        zip_files = []
        for fname in os.listdir(backup_dir):
            if fname.endswith(".zip"):
                fpath = os.path.join(backup_dir, fname)
                try:
                    mtime = os.path.getmtime(fpath)
                    zip_files.append((mtime, fpath))
                except OSError:
                    pass

        if len(zip_files) <= max_versions:
            return 0

        # 按时间升序（最旧的在前），删除多余的
        zip_files.sort(key=lambda x: x[0])
        to_delete = zip_files[:len(zip_files) - max_versions]

        deleted = 0
        for _, fpath in to_delete:
            try:
                os.remove(fpath)
                logger.info("已删除旧备份: %s", fpath)
                deleted += 1
            except Exception as e:
                logger.warning("删除旧备份失败 %s: %s", fpath, e)

        return deleted

    # ========== 完整备份 ==========

    def run_backup(self) -> dict:
        """
        执行一次完整的备份流程：
        1. 检测变化的存档
        2. 逐一对变化存档执行备份
        3. 清理所有存档的旧备份
        返回状态 dict
        """
        with self._lock:
            status = {
                "time": datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
                "total": 0,
                "backed_up": 0,
                "failed": 0,
                "cleaned": 0,
                "message": "",
            }

            try:
                logger.info("=== 开始备份周期 ===")

                # 1. 检测变化
                changed = self.check_saves_changed()
                status["total"] = len(changed)

                if not changed:
                    status["message"] = "没有检测到变化的存档"
                    logger.info(status["message"])
                else:
                    # 2. 执行备份
                    for map_name, save_name, save_path, relative_key in changed:
                        success = self.backup_save(map_name, save_name, save_path, relative_key)
                        if success:
                            status["backed_up"] += 1
                        else:
                            status["failed"] += 1

                    # 3. 清理旧备份（对所有存档执行）
                    all_saves = self.scan_saves()
                    for _, _, save_path, _ in all_saves:
                        status["cleaned"] += self.cleanup_old_backups(save_path)

                    status["message"] = (
                        f"备份完成: 检测到 {status['total']} 个变化存档, "
                        f"成功 {status['backed_up']}, 失败 {status['failed']}, "
                        f"清理 {status['cleaned']} 个旧备份"
                    )

                logger.info(status["message"])

            except Exception as e:
                status["message"] = f"备份过程出错: {e}"
                logger.error(status["message"], exc_info=True)

            # 回调通知
            if self._status_callback:
                try:
                    self._status_callback(status)
                except Exception:
                    pass

            return status

    # ========== 获取所有存档的备份信息 ==========

    def get_all_saves_info(self):
        """获取所有存档的备份状态信息，供 GUI 显示"""
        info_list = []
        all_saves = self.scan_saves()
        for map_name, save_name, save_path, relative_key in all_saves:
            last_backup_str = self._config.get_last_backup_time(relative_key)
            last_modified = self.get_save_last_modified(save_path)
            info_list.append({
                "map_name": map_name,
                "save_name": save_name,
                "save_path": save_path,
                "relative_key": relative_key,
                "monitored": self._config.is_monitored(relative_key),
                "last_backup": last_backup_str or "从未备份",
                "last_modified": last_modified.strftime("%Y-%m-%d %H:%M:%S") if last_modified else "未知",
            })
        return info_list

    # ========== 备份列表 ==========

    def get_save_backups(self, save_path: str) -> list:
        """
        获取某个存档的所有备份文件列表
        返回: [{"filename": str, "path": str, "size_mb": float, "time": str}, ...]
        """
        backup_dir = os.path.join(save_path, "backup")
        if not os.path.isdir(backup_dir):
            return []

        backups = []
        for fname in os.listdir(backup_dir):
            if not fname.endswith(".zip"):
                continue
            fpath = os.path.join(backup_dir, fname)
            try:
                stat = os.stat(fpath)
                backups.append({
                    "filename": fname,
                    "path": fpath,
                    "size_mb": round(stat.st_size / (1024 * 1024), 1),
                    "time": datetime.fromtimestamp(stat.st_mtime).strftime("%Y-%m-%d %H:%M:%S"),
                    "timestamp": stat.st_mtime,
                })
            except OSError:
                pass

        # 按时间降序（最新的在前）
        backups.sort(key=lambda x: x["timestamp"], reverse=True)
        return backups

    # ========== 恢复 ==========

    def restore_backup(self, zip_path: str, save_path: str) -> bool:
        """
        从备份 zip 恢复存档
        - 先创建"恢复前快照"备份
        - 清除存档目录中的非backup文件
        - 解压 zip 覆盖
        返回是否成功
        """
        try:
            logger.info("开始恢复存档: %s -> %s", zip_path, save_path)

            # 1. 创建恢复前快照（安全网）
            timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
            snapshot_name = f"RESTORE_SNAPSHOT_{timestamp}"
            self.backup_save("SNAPSHOT", snapshot_name, save_path, "")

            # 2. 清除存档目录（保留 backup 目录）
            for item in os.listdir(save_path):
                if item == "backup":
                    continue
                item_path = os.path.join(save_path, item)
                try:
                    if os.path.isdir(item_path):
                        shutil.rmtree(item_path)
                    else:
                        os.remove(item_path)
                except Exception as e:
                    logger.warning("删除失败 %s: %s", item_path, e)

            # 3. 解压备份
            with zipfile.ZipFile(zip_path, "r") as zf:
                zf.extractall(save_path)

            logger.info("恢复完成: %s", zip_path)
            return True

        except Exception as e:
            logger.error("恢复失败: %s", e, exc_info=True)
            return False
