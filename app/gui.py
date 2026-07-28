"""
GUI 模块 - tkinter 设置窗口
- 存档列表（带勾选框，折叠状态保持）
- 备份历史列表 + 恢复按钮
- 设置实时保存
- 动态刷新
"""
import tkinter as tk
from tkinter import ttk, messagebox
import threading
import os
import sys
import logging

logger = logging.getLogger("BackupTool")

CHECK_ON = "☑"
CHECK_OFF = "☐"


class BackupGUI:
    """主设置窗口"""

    def __init__(self, config_manager, backup_engine, scheduler, startup_manager):
        self._config = config_manager
        self._engine = backup_engine
        self._scheduler = scheduler
        self._startup_mgr = startup_manager

        self._root = None
        self._tree = None
        self._backup_listbox = None
        self._status_label = None
        self._next_label = None
        self._interval_var = None
        self._versions_var = None
        self._autostart_var = None
        self._select_all_var = None          # 全选勾选变量
        self._exit_callback = None
        self._selected_save_path = None
        self._selected_relative_key = None
        self._backup_data = []
        self._selected_backup_idx = -1       # 保持选中状态跨刷新
        self._refresh_timer_id = None
        self._auto_refresh = True
        self._expanded_maps = set()          # 记录折叠状态：地图名集合

    def set_exit_callback(self, fn):
        self._exit_callback = fn

    # ========== 窗口控制 ==========

    def setup(self):
        """初始化窗口（创建但不显示）"""
        self._root = tk.Tk()
        self._root.title("7 Days to Die 存档备份工具")

        # 设置窗口图标
        try:
            if getattr(sys, 'frozen', False):
                base = sys._MEIPASS
            else:
                base = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
            ico_path = os.path.join(base, "resources", "icon.ico")
            if os.path.exists(ico_path):
                self._root.iconbitmap(ico_path)
        except Exception:
            pass  # 图标加载失败不阻塞启动
        self._root.geometry("780x580")
        self._root.minsize(600, 450)
        self._root.protocol("WM_DELETE_WINDOW", self._on_close)

        self._root.update_idletasks()
        sw = self._root.winfo_screenwidth()
        sh = self._root.winfo_screenheight()
        w, h = 780, 580
        x = (sw - w) // 2
        y = (sh - h) // 2
        self._root.geometry(f"{w}x{h}+{x}+{y}")

        self._build_ui()
        self._root.withdraw()
        self._start_auto_refresh()

    def show(self):
        if self._root:
            self._refresh_data()
            self._root.deiconify()
            self._root.lift()
            self._root.focus_force()

    def hide(self):
        if self._root:
            self._root.withdraw()

    def run_mainloop(self):
        if self._root:
            self._root.mainloop()

    def is_visible(self):
        return self._root and self._root.state() != "withdrawn"

    # ========== UI 构建 ==========

    def _build_ui(self):
        root = self._root
        main_frame = ttk.Frame(root, padding=10)
        main_frame.pack(fill=tk.BOTH, expand=True)

        # ==== 状态栏 ====
        status_frame = ttk.LabelFrame(main_frame, text="状态", padding=6)
        status_frame.pack(fill=tk.X, pady=(0, 8))

        self._status_label = ttk.Label(status_frame, text="就绪", font=("", 9))
        self._status_label.pack(anchor=tk.W)
        self._next_label = ttk.Label(status_frame, text="下次备份: --", font=("", 9))
        self._next_label.pack(anchor=tk.W)

        # ==== 中间区域：左侧存档树 + 右侧备份历史 ====
        mid_pw = ttk.PanedWindow(main_frame, orient=tk.HORIZONTAL)
        mid_pw.pack(fill=tk.BOTH, expand=True, pady=(0, 8))

        # 左侧：存档树
        left_frame = ttk.LabelFrame(mid_pw, text="存档列表（勾选的计入备份）", padding=6)
        mid_pw.add(left_frame, weight=3)

        # 全选勾选框
        select_all_bar = ttk.Frame(left_frame)
        select_all_bar.pack(fill=tk.X, pady=(0, 4))
        self._select_all_var = tk.BooleanVar(value=True)
        ttk.Checkbutton(
            select_all_bar, text="全选 / 取消全选", variable=self._select_all_var,
            command=self._on_select_all,
        ).pack(side=tk.LEFT)

        tree_frame = ttk.Frame(left_frame)
        tree_frame.pack(fill=tk.BOTH, expand=True)

        self._tree = ttk.Treeview(
            tree_frame,
            columns=("last_backup", "last_modified"),
            show="tree headings",
            selectmode="browse",
        )
        self._tree.heading("#0", text="地图 / 存档")
        self._tree.heading("last_backup", text="上次备份")
        self._tree.heading("last_modified", text="最后修改")
        self._tree.column("#0", width=200, minwidth=140)
        self._tree.column("last_backup", width=150, anchor=tk.CENTER)
        self._tree.column("last_modified", width=140, anchor=tk.CENTER)

        tree_scroll = ttk.Scrollbar(tree_frame, orient=tk.VERTICAL, command=self._tree.yview)
        self._tree.configure(yscrollcommand=tree_scroll.set)
        self._tree.pack(side=tk.LEFT, fill=tk.BOTH, expand=True)
        tree_scroll.pack(side=tk.RIGHT, fill=tk.Y)

        self._tree.bind("<ButtonRelease-1>", self._on_tree_click)
        self._tree.bind("<<TreeviewSelect>>", self._on_tree_select)

        # 右侧：备份历史
        right_frame = ttk.LabelFrame(mid_pw, text="备份历史（选中存档后显示）", padding=6)
        mid_pw.add(right_frame, weight=2)

        # 列表容器（listbox + scrollbar）
        list_container = ttk.Frame(right_frame)
        list_container.pack(fill=tk.BOTH, expand=True)

        self._backup_listbox = tk.Listbox(
            list_container, font=("Consolas", 9), selectmode=tk.SINGLE,
            exportselection=False,
        )
        list_scroll = ttk.Scrollbar(list_container, orient=tk.VERTICAL, command=self._backup_listbox.yview)
        self._backup_listbox.configure(yscrollcommand=list_scroll.set)
        self._backup_listbox.pack(side=tk.LEFT, fill=tk.BOTH, expand=True)
        list_scroll.pack(side=tk.RIGHT, fill=tk.Y)

        self._backup_listbox.insert(tk.END, "（请先在左侧点击存档）")

        # 恢复按钮 — 在列表容器下方，与左侧存档树底部对齐
        ttk.Button(
            right_frame, text="恢复选中备份",
            command=self._on_restore_backup,
        ).pack(fill=tk.X, pady=(4, 0))

        # ==== 设置区（按钮内嵌在右侧）====
        settings_frame = ttk.LabelFrame(main_frame, text="设置（实时保存）", padding=8)
        settings_frame.pack(fill=tk.X, pady=(0, 8))

        row1 = ttk.Frame(settings_frame)
        row1.pack(fill=tk.X, pady=(0, 4))

        ttk.Label(row1, text="备份间隔 (分钟):").pack(side=tk.LEFT, padx=(0, 4))
        self._interval_var = tk.IntVar(
            value=self._config.get("backup_interval_seconds", 300) // 60
        )
        interval_spin = ttk.Spinbox(
            row1, from_=1, to=60, width=5,
            textvariable=self._interval_var,
            command=self._on_setting_changed,
        )
        interval_spin.pack(side=tk.LEFT, padx=(0, 20))
        interval_spin.bind("<FocusOut>", lambda e: self._on_setting_changed())
        interval_spin.bind("<Return>", lambda e: self._on_setting_changed())

        ttk.Label(row1, text="保留版本数:").pack(side=tk.LEFT, padx=(0, 4))
        self._versions_var = tk.IntVar(
            value=self._config.get("max_backup_versions", 10)
        )
        versions_spin = ttk.Spinbox(
            row1, from_=1, to=100, width=5,
            textvariable=self._versions_var,
            command=self._on_setting_changed,
        )
        versions_spin.pack(side=tk.LEFT, padx=(0, 20))
        versions_spin.bind("<FocusOut>", lambda e: self._on_setting_changed())
        versions_spin.bind("<Return>", lambda e: self._on_setting_changed())

        self._autostart_var = tk.BooleanVar(value=self._startup_mgr.is_enabled())
        ttk.Checkbutton(
            row1, text="开机自启", variable=self._autostart_var,
            command=self._on_autostart_toggle,
        ).pack(side=tk.LEFT, padx=(0, 20))

        # 按钮 — 放在设置行右侧
        ttk.Button(row1, text="立即备份", command=self._on_backup_now).pack(side=tk.RIGHT, padx=(4, 0))
        ttk.Button(row1, text="隐藏窗口", command=self.hide).pack(side=tk.RIGHT, padx=(4, 0))

        # ==== 版权 ====
        copyright_lbl = ttk.Label(
            main_frame, text="Developed by Lumore",
            font=("", 8), foreground="gray",
        )
        copyright_lbl.pack(side=tk.BOTTOM, pady=(4, 0))

    # ========== 全选 ==========

    def _on_select_all(self):
        """全选 / 取消全选所有存档"""
        select_all = self._select_all_var.get()
        saves_info = self._engine.get_all_saves_info()
        for s in saves_info:
            self._config.set_monitored(s["relative_key"], select_all)
        self._refresh_data()

    # ========== 数据刷新（保持折叠状态）==========

    def _refresh_data(self):
        """刷新存档树、备份列表、状态（保持展开/折叠状态）"""
        try:
            selected_key = self._selected_relative_key

            # ---- 保存展开状态 ----
            saved_expanded = set()
            for item_id in self._tree.get_children():
                if self._tree.item(item_id, "open"):
                    saved_expanded.add(self._tree.item(item_id, "text"))

            # 清空树
            for item in self._tree.get_children():
                self._tree.delete(item)

            # 获取存档信息
            saves_info = self._engine.get_all_saves_info()

            # 首次运行自动全选（仅执行一次）
            if not self._config.get("monitored_initialized") and len(saves_info) > 0:
                for s in saves_info:
                    self._config.set_monitored(s["relative_key"], True)
                self._config.set("monitored_initialized", True)

            # 按地图分组
            maps = {}
            for info in saves_info:
                mn = info["map_name"]
                if mn not in maps:
                    maps[mn] = []
                maps[mn].append(info)

            # 填充树
            for map_name, saves in sorted(maps.items()):
                is_open = map_name in saved_expanded if saved_expanded else True
                map_id = self._tree.insert(
                    "", tk.END, text=map_name, open=is_open, values=("", "")
                )
                for s in saves:
                    chk = CHECK_ON if s["monitored"] else CHECK_OFF
                    last_bk = s["last_backup"]
                    if last_bk and last_bk != "从未备份":
                        try:
                            parts = last_bk.split("T")
                            if len(parts) == 2:
                                last_bk = f"{parts[0]} {parts[1][:8]}"
                        except Exception:
                            pass

                    item_id = self._tree.insert(
                        map_id, tk.END,
                        text=f"{chk} {s['save_name']}",
                        values=(last_bk, s["last_modified"]),
                        tags=(s["relative_key"],),
                    )

                    if selected_key and s["relative_key"] == selected_key:
                        self._tree.selection_set(item_id)

            # 更新状态栏
            self._status_label.config(text=f"存档路径: {self._config.save_path}")
            interval = self._config.get("backup_interval_seconds", 300)
            self._next_label.config(
                text=f"备份间隔: {interval} 秒 | 保留版本: {self._config.get('max_backup_versions', 10)}"
            )

            self._refresh_backup_list()

        except Exception as e:
            logger.error("刷新数据失败: %s", e)

    def _toggle_checkbox(self, item_id):
        """切换存档的勾选状态"""
        text = self._tree.item(item_id, "text")
        tags = self._tree.item(item_id, "tags")
        if not tags:
            return

        relative_key = tags[0]
        if text.startswith(CHECK_ON):
            new_text = text.replace(CHECK_ON, CHECK_OFF, 1)
            self._config.set_monitored(relative_key, False)
        else:
            new_text = text.replace(CHECK_OFF, CHECK_ON, 1)
            self._config.set_monitored(relative_key, True)

        self._tree.item(item_id, text=new_text)

    def _refresh_backup_list(self):
        """刷新右侧备份历史列表（保持用户选中状态）"""
        # 保存当前选中
        saved_idx = -1
        sel = self._backup_listbox.curselection()
        if sel:
            saved_idx = sel[0]

        self._backup_listbox.delete(0, tk.END)
        self._backup_data = []

        if not self._selected_save_path or not os.path.isdir(self._selected_save_path):
            self._backup_listbox.insert(tk.END, "（请先在左侧点击存档）")
            return

        backups = self._engine.get_save_backups(self._selected_save_path)
        if not backups:
            self._backup_listbox.insert(tk.END, "（暂无备份）")
            return

        self._backup_data = backups
        for b in backups:
            self._backup_listbox.insert(
                tk.END,
                f"{b['time']}  |  {b['size_mb']:>6.1f} MB  |  {b['filename']}"
            )

        # 恢复选中（索引不超出新列表范围）
        if 0 <= saved_idx < len(backups):
            self._backup_listbox.selection_set(saved_idx)
            self._backup_listbox.activate(saved_idx)

    # ========== 动态刷新 ==========

    def _start_auto_refresh(self):
        """启动定时动态刷新（每3秒）"""
        self._start_refresh_cycle()

    def _start_refresh_cycle(self):
        if not self._auto_refresh or not self._root:
            return
        if self._root.state() != "withdrawn":
            self._refresh_data()
        self._refresh_timer_id = self._root.after(3000, self._start_refresh_cycle)

    def _stop_auto_refresh(self):
        self._auto_refresh = False
        if self._refresh_timer_id:
            self._root.after_cancel(self._refresh_timer_id)
            self._refresh_timer_id = None

    # ========== 树点击 / 选择事件 ==========

    def _on_tree_click(self, event):
        """点击复选框区域 → 切换勾选；点击名称 → 仅选中"""
        item_id = self._tree.identify_row(event.y)
        if not item_id:
            return

        parent = self._tree.parent(item_id)
        if not parent:
            return  # 地图节点

        # 判断点击位置是否在复选框区域（前 ~30px）
        bbox = self._tree.bbox(item_id, "#0")
        if bbox and event.x < bbox[0] + 30:
            self._toggle_checkbox(item_id)

    def _on_tree_select(self, event):
        """选中节点 -> 显示备份历史"""
        sel = self._tree.selection()
        if not sel:
            return
        item_id = sel[0]
        parent = self._tree.parent(item_id)
        if not parent:
            self._selected_relative_key = None
            self._selected_save_path = None
            self._refresh_backup_list()
            return

        tags = self._tree.item(item_id, "tags")
        if not tags:
            return

        relative_key = tags[0]
        saves_info = self._engine.get_all_saves_info()
        for info in saves_info:
            if info["relative_key"] == relative_key:
                self._selected_relative_key = relative_key
                self._selected_save_path = info["save_path"]
                self._refresh_backup_list()
                break

    # ========== 恢复备份 ==========

    def _on_restore_backup(self):
        """恢复选中备份"""
        if not self._selected_save_path:
            messagebox.showwarning("提示", "请先在左侧选择要恢复的存档", parent=self._root)
            return

        sel = self._backup_listbox.curselection()
        if not sel:
            messagebox.showwarning("提示", "请在右侧选择一个备份文件", parent=self._root)
            return

        idx = sel[0]
        if idx >= len(self._backup_data):
            return

        backup = self._backup_data[idx]

        result = messagebox.askyesno(
            "确认恢复",
            f"即将恢复备份：\n\n"
            f"  存档：{self._selected_relative_key}\n"
            f"  备份：{backup['filename']}\n"
            f"  时间：{backup['time']}\n"
            f"  大小：{backup['size_mb']} MB\n\n"
            f"  恢复会覆盖当前存档！\n"
            f"  系统将自动创建恢复前快照备份。\n\n"
            f"确定要恢复吗？",
            parent=self._root,
            icon="warning",
        )

        if not result:
            return

        def do_restore():
            self.schedule_ui_update(
                lambda: self._status_label.config(text="正在恢复存档...")
            )
            ok = self._engine.restore_backup(backup["path"], self._selected_save_path)
            if ok:
                self.schedule_ui_update(
                    lambda: messagebox.showinfo("恢复完成", "存档已成功恢复！", parent=self._root)
                )
            else:
                self.schedule_ui_update(
                    lambda: messagebox.showerror("恢复失败", "恢复过程中出现错误，请查看日志。", parent=self._root)
                )
            self.schedule_ui_update(self._refresh_data)

        threading.Thread(target=do_restore, daemon=True).start()

    # ========== 线程安全的 UI 更新 ==========

    def schedule_ui_update(self, fn, *args, **kwargs):
        if self._root:
            self._root.after(0, lambda: fn(*args, **kwargs))

    def update_status(self, text: str):
        def _update():
            self._status_label.config(text=text)
        self.schedule_ui_update(_update)

    def update_next_backup(self, text: str):
        def _update():
            self._next_label.config(text=text)
        self.schedule_ui_update(_update)

    def refresh_list(self):
        self.schedule_ui_update(self._refresh_data)

    def show_notification(self, title: str, msg: str):
        def _notify():
            if self._root and self._root.state() != "withdrawn":
                messagebox.showinfo(title, msg, parent=self._root)
        self.schedule_ui_update(_notify)

    # ========== 设置实时保存 ==========

    def _on_setting_changed(self):
        """设置变更时实时保存"""
        try:
            interval_min = self._interval_var.get()
            versions = self._versions_var.get()

            if interval_min < 1:
                interval_min = 1
                self._interval_var.set(1)
            if versions < 1:
                versions = 1
                self._versions_var.set(1)

            self._config.update({
                "backup_interval_seconds": interval_min * 60,
                "max_backup_versions": versions,
            })
            self._scheduler.update_interval(interval_min * 60)
            self._refresh_data()

        except Exception as e:
            logger.error("保存设置失败: %s", e)

    def _on_autostart_toggle(self):
        enable = self._autostart_var.get()
        ok, msg = self._startup_mgr.toggle(enable)
        if not ok:
            self._autostart_var.set(not enable)
            if self._root.state() != "withdrawn":
                messagebox.showerror("错误", msg, parent=self._root)

    # ========== 按钮事件 ==========

    def _on_backup_now(self):
        self._status_label.config(text="正在备份...")
        self._next_label.config(text="请稍候...")

        def do_backup():
            status = self._scheduler.run_once()
            self.schedule_ui_update(
                lambda: self._status_label.config(text=status.get("message", "完成"))
            )
            self.schedule_ui_update(self._refresh_data)

        threading.Thread(target=do_backup, daemon=True).start()

    def _on_close(self):
        if self._config.get("minimize_to_tray", True):
            self.hide()
        else:
            if self._exit_callback:
                self._exit_callback()
