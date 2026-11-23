# Revolc 待办

> 排序 = 建议执行顺序。历史项与踩坑记录见 `../Revolc_交接文档.md`，行为规格见
> `Rebuild_Target_Spec.md`（里程碑 M0–M6），本文件只留“还没做、以及为什么做”。

> **2026-11-23 更新（已合并入 master，合并提交 `a9874a6`）**：收藏栏与标签栏已合并为一条
> 62px chrome 条、不可隐藏；右端新增设置按钮，点开一个 `Painted` 设置标签页（单例），
> 可切换接管模式 / 主题预设 / 标签圆角，改动按行写回 TOML 并热生效。抽象见
> `ui/view.rs`（`Pending / Hosted / Painted`）与 `docs/Chrome一体化与设置页改造计划.md`。
> 本文件下面的历史项仍有效；其中「收藏栏可关闭」相关描述已被本次改动取代。

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
4b. **标签右键菜单（2026-11-20 分支 `feat/tab-context-menu`）**：
   - ✅ 一期（`f175501`）：菜单骨架 + 在新窗口打开 / 关闭 / 关其他 / 关右侧 / 右侧新建。
   - ✅ 二期（`694371f`）：收藏栏 UI 空壳（`[bookmarks] enabled` 开关，热重载；独立 owned popup）。
     **2026-11-23 起已变更**：收藏栏并入标签栏的 62px chrome 条、不可隐藏，
     该开关不再控制显隐；见 `docs/Chrome一体化与设置页改造计划.md`。
   - ✅ 2.5（`5879be7`~`2dc9f91`）：**BHO 导航事件推送**——DWebBrowserEvents2 订阅 +
     WM_COPYDATA 推送 + **label 匹配**。踩坑记录（重要）：
     ①DISPID 250=BeforeNavigate2，NavigateComplete2=**252**，DocumentComplete=**259**；
     ②窗口被 SetParent 吸收后 `get_HWND` 返回宿主 RevolcMain（父链无 CabinetWClass）——
     HWND/pid 匹配均不可靠（pid 理论 1:1 但手测仍偶发失配）；
     ③**tab label（窗口标题）是唯一从未出错的实时信号** → 推送 URL 尾段 == label 配对，
     歧义取活动 tab，label 滞后由 sweep 重放缓存兜底；
     ④SetSite 期间禁止调用 site 方法（订阅全部延迟到 timer 回调）。
     `[bookmarks].enabled` → tray 启动时写/删 `bho.nav` 标记驱动订阅。
   - ✅ 三期（2026-11-23 分支 `feat/bookmarks-phase3`）：**扁平书签 + 工具栏芯片**。
     - 数据：`src/bookmarks.rs`，`Bookmark { label, path }` 扁平列表，schema 带
       `version=1`，存 `%APPDATA%\revolc\bookmarks.json`；原子写（`.tmp` →
       `sync_all` → `MoveFileExW(REPLACE_EXISTING)`），损坏/未来版本启动不动
       崩溃、回退空列表。路径按大小写/斜杠/尾分隔符归一化去重。5 个单测。
     - 交互：标签右键「收藏此标签页 / 取消收藏」（只对 Hosted 文件夹 tab 出现，
       用实时 BHO 路径，设置页不提供）；工具栏行左起画书签芯片（92px 固定宽、
       自适应数量、省略号截断），点芯片新开 tab；右端设置齿轮不变。
     - 书签芯片右键菜单（同分支补做）：「打开此收藏 / 取消收藏」——直接从收藏栏
       管理，不必先打开成 tab 再右键。`BookmarkStore::remove_at(index)` 按芯片索引删。
     - **拖拽重排（与标签栏共用一套逻辑）**：抽出 `src/ui/reorder.rs`（`Drag`/`Press`/
       `landing()` 中心线落点，参数化 origin/width/gap）；`tabbar.rs` 的 Drag 与 landing
       改为复用它，行为零变化；书签栏用同款 press→阈值→drag→`move_to` 三段式，
       拖到的新顺序即时写回 `bookmarks.json`。App 层 `bookmark_press`/`bookmark_drag`
       两个状态字段，消息路由在 WM_LBUTTONDOWN/MOUSEMOVE/LBUTTONUP/CAPTURECHANGED。
       单击仍打开（按下即记录 press，抬起未跨阈值才算 click），拖不被误判为打开。
       顺带收紧双击最大化门禁到 caption 行——此前双击书签芯片会最大化窗口。
     - **悬停高亮（同分支补做）**：书签芯片与标签栏「+」按钮都加鼠标悬停反馈
       （复用主题 `tab_hover` 配色）。`tabbar.rs` 加 `hover_plus` 状态与 setter；
       App 的 `update_hover` 按垂直分区解析 tab/+/书签三种 hover（互斥），
       拖拽期间抑制、离开窗口 `clear_hover` 清除、书签增删后清索引防错位。
     - **UI 交互一致性审查（同分支补做）**：五个 chrome 控件里此前只有设置齿轮
       缺悬停反馈，本轮补上（App `settings_hover`，工具栏右端命中时填 `tab_hover`
       底色，与 caption 按钮一致）。同时消除两类重复：① 新建 `src/ui/gdi.rs`
       收敛 `rgb()`（RGB→COLORREF 0x00BBGGRR，原散落 4 文件 11 处）与 `line()`
       （caption/chrome 各一份）；② `chrome::paint` 的交互状态打包为
       `PaintState{drag,hover,settings_hover}`，App 统一用 `chrome_paint_state()`
       构造（原两处 paint 路径各写一遍）。
     - **审查发现但未做（待定）**：设置页（Painted tab）右侧的设置行没有悬停
       反馈——左侧分类有选中态、右侧行没有；且 Painted 页目前不接收
       WM_MOUSEMOVE 路由。属于新一层交互，未在本次 chrome 条一致性范围内。
     - **悬停对比度修复（用户手测反馈）**：绿色主题下设置齿轮悬停底色是浅绿
       `tab_hover`(#c8e6c9)，而 glyph 用浅绿 `tabbar_text`，几乎看不见。把
       `settings.rs` 原有的 `ink_on`（按背景亮度选黑/白字）提升为
       `gdi::contrast_text()` 公共函数，设置齿轮与书签芯片的 hover 文字都改用它
       （不再盲用主题色）；非 hover 态仍用主题色不变。3 个单测覆盖 RGB 字节序、
       浅绿底→深字、深底→浅字。日志复核零 WARN/ERROR。
     - 顺带修：设置页（Painted tab）活动时新建 tab 曾把字面量"设置"当文件夹
       路径 spawn（log 实锤 `explorer spawned for absorb path="设置"`）；
       `active_folder_path` 现在对非 Hosted tab 回退 home。
     - 范围外：文件夹层级、pin（四期）。当前实现未设数量上限，窗口宽度决定
       可见芯片数，超出的等窗口变宽才显示。
   - ⬜ 四期：固定标签页(pin)。
   **「重新打开关闭的标签页」明确不做**（浏览器式 reopen 需要 close stack：在 `close_tab`
   里存 path+label，还得跟 discard/reserve 竞态协调；当前 `Tab` 被直接丢弃，无此基础设施）。
   若日后要做：见 `src/tabs/mod.rs::Tab` 与 `src/ui/mod.rs::close_tab`。
5. **M3 第一阶段（已做，分支 `m3-integrated-titlebar`）：Chrome 式一体化标题栏**
   —— 窗口去掉 `WS_CAPTION`，`WM_NCCALCSIZE` 返回 0（客户区 = 窗口矩形），
   标题栏由 owned popup 绘制（`ui/tabbar.rs` 标签条 + `ui/caption.rs` 三个按钮），
   `WM_NCHITTEST` 回答 `HTCAPTION`（空白条带）+ 8 条缩放边框；三个按钮报 `HTCLIENT`
   自己处理点击（报 `HTMIN/MAX/CLOSE` 会触发 Win11 Snap Layouts 浮层错位，实测）。
   tab 条 34px、圆角只在顶部、底部与内容区齐平。主题新增 `caption_hover` / `caption_close_hover`。
   **2026-11-23 扩展**：工具栏并入同一 popup（总 62px），见文件头更新说明。
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
