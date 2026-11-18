# Revolc 待办

> 排序 = 建议执行顺序。历史项与踩坑记录见 `../Revolc_交接文档.md`，行为规格见
> `Rebuild_Target_Spec.md`（里程碑 M0–M6），本文件只留“还没做、以及为什么做”。

## P0 — 已知缺陷

1. **启动期吸收「已打开的窗口」是串行的，每个窗口约 100–150ms**
   （2026-11-18 实测 log：`hide → style → SetParent → framechanged` 每步 20–40ms，
   跨进程同步调用的固有代价）。4 个窗口 ≈ 0.5s，视觉上像“一个个被吞进去”。
   想法：吸收与摆位拆成两步（SetParent 用 `SWP_ASYNCWINDOWPOS` 之后不再同步等），
   或者按窗口 post 一个工作项、下一个窗口的 SetParent 与上一个的摆位重叠。
   非阻塞的替代：启动时先给所有待吸收窗口套一层遮罩（见 P2）。
2. **`startup path` 不做校验**：`revolc.exe Explorer` 会把字面量 `"Explorer"`
   当路径 spawn（log 实锤 `2026-11-17T00:40:30 path="Explorer"`）。
   修法：`initial_path` 必须通过 `GetFileAttributesW` / 存在性检查，否则回退 `home_dir()`。
3. **soak 测试**（交接文档 §6-1）：日常使用 1–2 天，重点开关机/注销/强杀后无僵尸窗口，
   以及“连开 10 个文件夹是否每个都进 tab”。

## P1 — 里程碑主线

4. ~~M1 会话持久化~~ / ~~M2 书签栏~~：**缓办**（2026-11-18 决定先做 M3）。
5. **M3 第一阶段（已做，分支 `m3-integrated-titlebar`）：Chrome 式一体化标题栏**
   —— 窗口去掉 `WS_CAPTION`，`WM_NCCALCSIZE` 返回 0（客户区 = 窗口矩形），
   标题栏由主窗口自绘（`ui/caption.rs` 三个按钮 + `ui/tabbar.rs` 标签条），
   `WM_NCHITTEST` 自己回答 `HTCAPTION` / `HTMINBUTTON` / `HTMAXBUTTON` / `HTCLOSE`
   + 8 条缩放边框。tab 条 34px、圆角只在顶部、底部与内容区齐平。
   主题新增 `caption_hover` / `caption_close_hover`。
5b. **标签占满行为的两条支线（未做，先记下要改哪里）**：
   a) 单行缩窄 —— 现在已经是 `TAB_MIN_W=90` / `TAB_MAX_W=190` 的折中，要更彻底地缩窄得配合文字省略策略。
   b) 多行标签 —— 需要 **band 高度动态化**。目前当成常量 34 的地方：`ui/mod.rs` 的 `TABBAR_HEIGHT`
   （`view_rect`、`WM_NCHITTEST` 的条带判定、`invalidate_band`、吸收窗口的 rect.top）以及
   `ui/tabbar.rs` 里的 `TAB_BOTTOM` / `CENTER` / `hit()` 的 y 范围。改成 `TabBar::height()` 由
   `TabBar` 自己算（行数 × 行高 + 边距），其余地方都从它取即可。标签形状已经是"上圆下直"的独立区域，
   不依赖底下压着什么，多行不会露圆角。

6. **M3 第二阶段（未做）：tab 拖拽分窗/合并**（`Rebuild_Target_Spec.md` §3.1/§3.4）。
   现状是单窗口单进程，这一阶段要先把「多窗口」从 `App` 里拆出来（现在 `App` 持有
   唯一 `main_hwnd` + 一份 tabs），再做：tab 拖拽有插入位/落地预览、拖到同进程另一个
   窗口 = 合并、拖到桌面 = 新进程承接该 tab。
7. 之后 M4 主题系统（圆角/透明/配色 + 一体化标题栏的主题化）、M5 安装包、
   M6 接管完善（`associate` 模式目前未做）。

## P2 — 体验/性能想法池（未排期）

0. **标题栏三按钮走客户区，不用 `HTMIN/MAX/CLOSE`**（2026-11-18 决定）：把按钮上报成非客户区后，
   按下会走 `DefWindowProc`，在“没有非客户区”的窗口上系统给不出按钮位置，实测表现为点击无效 +
   按钮上冒出一个白色方块（本机 Win10；Win11 上还会牵出 Snap Layouts 浮层的锚点问题）。
   现在 hover 仍由 `WM_NCHITTEST` 算，点击是 `WM_LBUTTONDOWN` + `caption_action()`。
   代价：Win11 的 Snap Layouts 悬停浮层没有（本机 Win10，可忽略）；若将来要接，先解决锚点再谈。

6. **启动遮罩（障眼法）**：进程启动后立即铺一层与主题同色的窗口/分层位图，
   把所有窗口吸收完再淡出；用于掩盖 P0-1 的顺序感。
   进阶：吸收中的窗口位置用模糊/占位块盖住，等它真正上屏再揭开。
7. **吸收并行化**：需要先厘清哪些跨进程调用必须同步（`SetWindowPos` 同步是为了
   避免子窗口落后一帧，见 `host::place_client` 注释），哪些可以异步化。
8. **DWebBrowserEvents2 订阅**（交接文档 §6-2）：导航/标题毫秒级同步，
   现在靠 200ms 扫描读窗口标题，晚一帧。
9. **REA 对照 Clover 的三个细节**：点击 tab 区域时的 SetForegroundWindow 顺序、
   Alt+Tab 表现、关闭语义；以及吸收后 `Ctrl+Z` / 右键菜单 / OLE 拖放是否正常
   （输入队列只解决键盘）。
10. **发布形态**：release DLL 签名、安装器、注册表开关设置页、
    BHO 被安全软件禁用时的降级提示。

## 参数速查（调参先看这里）

`src/ui/mod.rs`：`SWEEP_FAST_MS`(15) / `SWEEP_IDLE_MS`(200) / `BURST_WINDOW`(1.5s) /
`EXCLUDE_TTL`(8s) / `RESERVE_TTL`(6s)。
`src/shell/host.rs`：`PRE_SHOW_GRACE`(1.5s，等 Explorer 自己 ShowWindow 的宽限期)。
