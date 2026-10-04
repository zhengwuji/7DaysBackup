"""
备份引擎模块
- 扫描存档目录结构
- 检测存档变化（基于备份时刻的文件清单 mtime+size 精确对比，不受时钟/DST 影响）
- 执行 zip 压缩备份（支持自定义输出位置、磁盘空间检查、被占用文件计数）
- 分层清理过期备份（保留最近 N 个 + 尾部每天最新 1 个额外保留 N 天）
- 恢复备份（zip 路径校验、恢复前快照、临时目录中转防半成品）
"""
import os
import shutil
import zipfile
import logging
import threading
from datetime import datetime

logger = logging.getLogger("BackupTool")

SNAPSHOT_DIR = "snapshots"      # 恢复前快照的子目录名（位于备份目录下）
MAX_SNAPSHOTS = 3               # 最多保留的恢复前快照数量


def _safe_zip_name(name: str) -> bool:
    """校验 zip 条目名，拒绝绝对路径与 .. 上跳（防 zip 路径穿越）"""
    n = name.replace("\\", "/")
    if not n or n.startswith("/") or (len(n) > 1 and n[1] == ":"):
        return False
    return ".." not in n.split("/")


def _is_under(path_abs: str, root_abs: str) -> bool:
    """判断 path 是否等于 root 或位于 root 之下（Windows 下忽略大小写）"""
    path_abs = os.path.normcase(path_abs)
    root_abs = os.path.normcase(root_abs)
    return path_abs == root_abs or path_abs.startswith(root_abs + os.sep)


class BackupEngine:
    """备份引擎，负责扫描、压缩、清理、恢复"""

    def __init__(self, config_manager):
        self._config = config_manager
        self._lock = threading.Lock()     # 保护 run_backup / restore_backup 互斥
        self._status_callback = None      # 状态变更回调: fn(status_dict)

    def set_status_callback(self, callback):
        """设置状态回调，用于通知 GUI 更新"""
        self._status_callback = callback

    # ========== 路径 ==========

    def get_backup_dir(self, map_name: str, save_name: str, save_path: str) -> str:
        """
        获取某存档的备份输出目录
        - 配置了 backup_output_path: {自定义目录}/{MapName}/{SaveName}/
        - 未配置（默认）: {save_path}/backup/
        """
        custom = (self._config.get("backup_output_path") or "").strip()
        if custom:
            return os.path.join(custom, map_name, save_name)
        return os.path.join(save_path, "backup")

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

    # ========== 文件收集 / 清单 ==========

    def _iter_save_files(self, save_path: str, exclude_dirs):
        """
        遍历存档目录，返回 [(full_path, relpath, mtime_ns, size), ...]
        排除 exclude_dirs 中的目录（含其子目录），以及任意名为 backup 的目录
        （备份输出无论在存档内还是存档外都不会混进备份/清单）
        """
        excludes = [os.path.abspath(d) for d in exclude_dirs if d]
        files = []
        for root, dirs, names in os.walk(save_path):
            pruned = []
            for d in dirs:
                full = os.path.abspath(os.path.join(root, d))
                if d == "backup":                          # 默认备份目录（含历史遗留）
                    continue
                if any(_is_under(full, e) for e in excludes):
                    continue
                pruned.append(d)
            dirs[:] = pruned

            for name in names:
                full = os.path.join(root, name)
                try:
                    st = os.stat(full)
                except OSError:
                    continue
                rel = os.path.relpath(full, save_path)
                files.append((full, rel, st.st_mtime_ns, st.st_size))
        return files

    @staticmethod
    def _to_manifest(file_list) -> dict:
        """文件列表 -> 清单 {relpath: [mtime_ns, size]}"""
        return {rel: [mt, size] for _, rel, mt, size in file_list}

    # ========== 变更检测 ==========

    def get_save_last_modified(self, save_path: str, exclude_dirs=None):
        """获取存档目录最新文件修改时间（排除备份目录），返回 datetime 或 None"""
        files = self._iter_save_files(save_path, exclude_dirs or [])
        if not files:
            return None
        return datetime.fromtimestamp(max(f[2] for f in files) / 1e9)

    def check_saves_changed(self):
        """
        检查所有被监控的存档，返回发生变化的存档列表
        对比当前文件清单与上次备份时记录的清单（mtime_ns + size），
        与系统时钟无关，且备份期间写入的文件不会漏检
        """
        changed = []
        monitored = set(self._config.get_monitored_saves())

        for map_name, save_name, save_path, relative_key in self.scan_saves():
            if relative_key not in monitored:
                continue

            backup_dir = self.get_backup_dir(map_name, save_name, save_path)
            current = self._to_manifest(self._iter_save_files(save_path, [backup_dir]))
            stored = self._config.get_manifest(relative_key)

            if stored is None or stored != current:
                changed.append((map_name, save_name, save_path, relative_key))

        return changed

    # ========== 备份 ==========

    def backup_save(self, map_name: str, save_name: str, save_path: str,
                    relative_key: str, snapshot: bool = False):
        """
        对单个存档执行备份
        - 先收集文件清单（排除备份目录），再压缩同一份列表
        - 清单以实际写入 zip 的文件为准：被占用跳过的文件不进清单，下次会自动重试
        - 先写临时文件再 os.replace，保证磁盘上的 zip 始终完整
        - snapshot=True 时输出到备份目录的 snapshots/ 子目录，不记录清单/备份时间
        返回 (是否成功, 跳过的文件数)
        """
        tmp_path = ""
        try:
            out_dir = self.get_backup_dir(map_name, save_name, save_path)
            if snapshot:
                out_dir = os.path.join(out_dir, SNAPSHOT_DIR)
            os.makedirs(out_dir, exist_ok=True)

            # 收集文件清单（排除备份输出目录）
            file_list = self._iter_save_files(save_path, [out_dir])

            # 生成备份文件名（临时文件放在地图目录下，避免 zip 自包含）
            dt = datetime.now()
            timestamp = dt.strftime("%Y%m%d_%H%M%S") + f"{dt.microsecond // 1000:03d}"
            zip_filename = f"{map_name}_{save_name}_{timestamp}.zip"
            map_dir = os.path.dirname(save_path)
            tmp_path = os.path.join(map_dir, f"_{zip_filename}.tmp")
            final_path = os.path.join(out_dir, zip_filename)

            logger.info("开始备份: %s/%s -> %s", map_name, save_name, final_path)

            # 磁盘空间检查（估算源大小 × 1.2，另留 10MB 余量）
            total_size = sum(size for _, _, _, size in file_list)
            try:
                free = shutil.disk_usage(map_dir).free
                if free < total_size * 1.2 + 10 * 1024 * 1024:
                    logger.error(
                        "磁盘空间不足（需要约 %.1f MB，剩余 %.1f MB），跳过备份 %s/%s",
                        total_size * 1.2 / 1048576, free / 1048576, map_name, save_name,
                    )
                    return False, 0
            except OSError:
                pass  # 无法获取剩余空间时不阻塞备份

            skipped = 0
            kept = []   # 实际写入 zip 的文件
            with zipfile.ZipFile(tmp_path, "w", zipfile.ZIP_DEFLATED) as zf:
                for full, rel, mt, size in file_list:
                    try:
                        zf.write(full, rel)
                        kept.append((full, rel, mt, size))
                    except OSError as e:
                        skipped += 1
                        logger.warning("跳过文件 %s: %s（可能被游戏占用）", full, e)

            os.replace(tmp_path, final_path)

            zip_size_mb = os.path.getsize(final_path) / (1024 * 1024)
            logger.info(
                "备份完成: %s (%d 个文件, 原始 %.1f MB, 压缩后 %.1f MB%s)",
                zip_filename, len(kept), total_size / 1048576, zip_size_mb,
                f", 跳过 {skipped} 个文件" if skipped else "",
            )

            if snapshot:
                self._cleanup_snapshots(out_dir)
                return True, skipped

            # 记录备份时间与文件清单（供显示 / 变更检测）
            self._config.set_last_backup_time(relative_key, datetime.now().isoformat())
            self._config.set_manifest(relative_key, self._to_manifest(kept))

            return True, skipped

        except Exception as e:
            logger.error("备份失败 %s/%s: %s", map_name, save_name, e, exc_info=True)
            try:
                if os.path.exists(tmp_path):
                    os.remove(tmp_path)
            except Exception:
                pass
            return False, 0

    def _cleanup_snapshots(self, snapshot_dir: str):
        """恢复前快照最多保留 MAX_SNAPSHOTS 个，删除更旧的"""
        try:
            snaps = []
            for fname in os.listdir(snapshot_dir):
                if fname.endswith(".zip"):
                    fpath = os.path.join(snapshot_dir, fname)
                    try:
                        snaps.append((os.path.getmtime(fpath), fpath))
                    except OSError:
                        pass
            snaps.sort(reverse=True)
            for _, fpath in snaps[MAX_SNAPSHOTS:]:
                try:
                    os.remove(fpath)
                    logger.info("已删除旧快照: %s", fpath)
                except OSError as e:
                    logger.warning("删除旧快照失败 %s: %s", fpath, e)
        except OSError as e:
            logger.warning("清理快照失败: %s", e)

    def cleanup_tmp_files(self):
        """清理异常退出残留的临时文件（地图目录下 _xxx.zip.tmp、_xxx_restore_tmp 目录）"""
        root = self._config.save_path
        if not os.path.isdir(root):
            return
        for map_name in os.listdir(root):
            map_dir = os.path.join(root, map_name)
            if not os.path.isdir(map_dir):
                continue
            try:
                entries = os.listdir(map_dir)
            except OSError:
                continue
            for name in entries:
                full = os.path.join(map_dir, name)
                try:
                    if name.startswith("_") and name.endswith(".tmp") and os.path.isfile(full):
                        os.remove(full)
                        logger.info("已清理残留临时文件: %s", full)
                    elif name.startswith("_") and name.endswith("_restore_tmp") and os.path.isdir(full):
                        shutil.rmtree(full, ignore_errors=True)
                        logger.info("已清理残留恢复临时目录: %s", full)
                except OSError as e:
                    logger.warning("清理临时文件失败 %s: %s", full, e)

    # ========== 清理 ==========

    def cleanup_old_backups(self, map_name: str, save_name: str, save_path: str) -> int:
        """
        分层清理旧备份：
        - 始终保留最新 N 个（max_backup_versions）
        - 更旧的备份中，每天最新 1 个额外保留 daily_keep_days 天（0=关闭）
        返回删除的文件数
        """
        max_versions = int(self._config.get("max_backup_versions", 10) or 10)
        daily_keep = int(self._config.get("daily_keep_days", 0) or 0)
        backup_dir = self.get_backup_dir(map_name, save_name, save_path)

        if not os.path.isdir(backup_dir):
            return 0

        zip_files = []
        for fname in os.listdir(backup_dir):
            if fname.endswith(".zip"):
                fpath = os.path.join(backup_dir, fname)
                try:
                    zip_files.append((os.path.getmtime(fpath), fpath))
                except OSError:
                    pass

        # 按时间降序（最新的在前）
        zip_files.sort(key=lambda x: x[0], reverse=True)

        if len(zip_files) <= max_versions:
            return 0

        keep_idx = set(range(max_versions))

        if daily_keep > 0:
            # 尾部（超出最近 N 个）中，每个自然日取最新的 1 个，最多保留 daily_keep 天
            kept_days = {}
            for i in range(max_versions, len(zip_files)):
                day = datetime.fromtimestamp(zip_files[i][0]).date()
                if day not in kept_days:
                    if len(kept_days) >= daily_keep:
                        break
                    kept_days[day] = i
                    keep_idx.add(i)

        deleted = 0
        for i, (_, fpath) in enumerate(zip_files):
            if i in keep_idx:
                continue
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
        3. 对所有存档执行分层清理
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
                else:
                    # 2. 执行备份
                    total_skipped = 0
                    for map_name, save_name, save_path, relative_key in changed:
                        ok, skipped = self.backup_save(map_name, save_name, save_path, relative_key)
                        if ok:
                            status["backed_up"] += 1
                        else:
                            status["failed"] += 1
                        total_skipped += skipped

                    # 3. 清理旧备份（对所有存档执行，保留策略变更后也能立即生效）
                    for map_name, save_name, save_path, _ in self.scan_saves():
                        status["cleaned"] += self.cleanup_old_backups(map_name, save_name, save_path)

                    status["message"] = (
                        f"备份完成: 检测到 {status['total']} 个变化存档, "
                        f"成功 {status['backed_up']}, 失败 {status['failed']}, "
                        f"清理 {status['cleaned']} 个旧备份"
                    )
                    if total_skipped:
                        status["message"] += f"，跳过 {total_skipped} 个被占用文件（下次自动重试）"

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
        for map_name, save_name, save_path, relative_key in self.scan_saves():
            backup_dir = self.get_backup_dir(map_name, save_name, save_path)
            last_backup_str = self._config.get_last_backup_time(relative_key)
            last_modified = self.get_save_last_modified(save_path, [backup_dir])
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

    def get_save_backups(self, map_name: str, save_name: str, save_path: str) -> list:
        """
        获取某个存档的所有备份文件列表（不含快照）
        返回: [{"filename": str, "path": str, "size_mb": float, "time": str}, ...]
        """
        backup_dir = self.get_backup_dir(map_name, save_name, save_path)
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

    def restore_backup(self, zip_path: str, map_name: str, save_name: str, save_path: str) -> bool:
        """
        从备份 zip 恢复存档（与定时备份互斥，全程持有引擎锁）
        1. 校验 zip 条目路径（防穿越），不通过直接失败
        2. 创建"恢复前快照"到备份目录的 snapshots/ 下（安全网）
        3. 解压到同卷临时目录，成功后再替换存档内容，避免半成品状态
        4. 恢复后记录文件清单，避免下一轮把刚恢复的内容重复备份
        """
        with self._lock:
            try:
                logger.info("开始恢复存档: %s -> %s", zip_path, save_path)

                backup_dir = self.get_backup_dir(map_name, save_name, save_path)

                # 1. 校验 zip（在任何删除操作之前）
                with zipfile.ZipFile(zip_path, "r") as zf:
                    bad = [i.filename for i in zf.infolist() if not _safe_zip_name(i.filename)]
                if bad:
                    logger.error("备份文件包含非法路径，已中止恢复: %s", bad[:5])
                    return False

                # 2. 恢复前快照（安全网，存到 snapshots/ 子目录）
                timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
                self.backup_save(
                    map_name, f"RESTORE_SNAPSHOT_{timestamp}", save_path,
                    relative_key="", snapshot=True,
                )

                # 3. 解压到临时目录（与存档同卷，可原子改名移动）
                tmp_extract = os.path.join(os.path.dirname(save_path), f"_{save_name}_restore_tmp")
                if os.path.exists(tmp_extract):
                    shutil.rmtree(tmp_extract, ignore_errors=True)
                os.makedirs(tmp_extract)
                try:
                    with zipfile.ZipFile(zip_path, "r") as zf:
                        zf.extractall(tmp_extract)

                    # 4. 清空存档目录（保留备份目录，含历史遗留 backup 目录）
                    for item in os.listdir(save_path):
                        item_path = os.path.join(save_path, item)
                        if item == "backup" or _is_under(os.path.abspath(item_path), backup_dir):
                            continue
                        try:
                            if os.path.isdir(item_path):
                                shutil.rmtree(item_path)
                            else:
                                os.remove(item_path)
                        except Exception as e:
                            logger.warning("删除失败 %s: %s", item_path, e)

                    # 5. 移入解压内容
                    for item in os.listdir(tmp_extract):
                        shutil.move(os.path.join(tmp_extract, item), os.path.join(save_path, item))
                finally:
                    shutil.rmtree(tmp_extract, ignore_errors=True)

                # 6. 记录恢复后的清单，避免恢复后立刻重复备份
                self._config.set_manifest(
                    f"{map_name}/{save_name}",
                    self._to_manifest(self._iter_save_files(save_path, [backup_dir])),
                )

                logger.info("恢复完成: %s", zip_path)
                return True

            except Exception as e:
                logger.error("恢复失败: %s", e, exc_info=True)
                return False
