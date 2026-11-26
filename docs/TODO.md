# Revolc 待办

> 排序 = 建议执行顺序。历史项与踩坑记录见 `交接文档.md`，行为规格见
> `Rebuild_Target_Spec.md`（里程碑 M0–M6），本文件只留“还没做、以及为什么做”。

> **2026-11-25 09:50 · 最终用户模型（取代下面 11-24 的三段式）**：用户只有两个开关。
> 「接管：是/否」是唯一主开关：是 = 首次 UAC 装 BHO，BHO 通知 + 后台扫描一起工作，
> 并广播一次吸收已有文件夹窗口；否 = 卸载清理（UAC 删注册表 / Run / `%APPDATA%\revolc`），
> 没有临时暂停状态。「开机自启：是/否」只有接管=是时可选，关接管自动关自启。
> 托盘在 Revolc 运行时常驻，不是开关；BHO/扫描/注册表都是内部实现，不再对用户暴露。
> 下面的「系统注册」行和 `SettingId::ToggleRegistry` 描述均为历史记录。
>
> 2026-11-25 10:15 · 行为切分（REA 依据 `clover/analysis/Clover_MultiWindow_And_Tray_Evidence.md` §8）：
> 接管=否时启动只开设置页（控制台），不建 Home tab、不 spawn Explorer；`+`/书签转设置页；
> X 强制退出不允许 hide。切到“是”时关掉设置 tab 并创建工作 tab；托盘广播吸收已有窗口。
> 托盘零窗口兜底改用 `--adopt` 启动：0 初始 tab，待吸收文件夹直接成为第一个 tab，
> 消除 Clover 式“新窗口 + 多余 Home tab”。

> **2026-11-23 更新（已合并入 master，合并提交 `a9874a6`）**：收藏栏与标签栏已合并为一条
> 62px chrome 条、不可隐藏；右端新增设置按钮，点开一个 `Painted` 设置标签页（单例），
> 可切换接管模式 / 主题预设 / 标签圆角，改动按行写回 TOML 并热生效。抽象见
> `ui/view.rs`（`Pending / Hosted / Painted`）与 `docs/Chrome一体化与设置页改造计划.md`。
> 本文件下面的历史项仍有效；其中「收藏栏可关闭」相关描述已被本次改动取代。

> **2026-11-23 下午 · 设置项集中化（已提交 `bb06392`）**：所有用户可见设置由
> `common/src/settings.rs::SettingId` 单一注册，新增 3 项：
> ① `[tray].autostart`（HKCU Run，任务管理器「启动应用」可见，启动 `revolc.exe`）；
> ② `[tray].enabled` 实时显隐托盘（关闭后托盘进程退出，主程序在重新打开时拉起）；
> ③ `[tray].close_action`（`exit` / `hide`；hide 只在托盘可见时生效，窗口 X 变为
> `SW_HIDE`，托盘「窗口列表」发 `WM_APP_RESTORE` 恢复）。`SettingId` 持有
> section/key/校验/cycle/预设编辑，设置页从 `SettingId::ALL` 生成行，不再硬编码
> `(section,key,value)`。主题预设迁至 `common/src/theme_presets.rs`。
> 真相源仍是 TOML：在任务管理器禁用自启项后，下次配置变更会把 Run 项写回。
>
> **2026-11-24 · 修 "托盘关不掉" 时的踩坑**：此前 `cargo build --release` 只重编了
> `revolc.exe`（因为只改了主程序源码），`revolc-tray.exe` 仍是旧二进制，导致新写的
> watcher 退出逻辑根本没跑——主程序侧一切正常，托盘侧毫无反应。排查手法：对比
> `target/release/revolc-tray.exe` 与 `tray/src/main.rs` 的 mtime。**验收任何涉及
> 两个 exe 的改动，必须确认两个二进制都重新构建**（`cargo build --release -p revolc -p revolc-tray`）。
> 另：托盘关不掉的根因代码是多个窗口进程无条件 spawn tray 的竞态，已改 `ensure_tray()`
> 先查 `RevolcResident` 窗口（commit `8cc61a0`）。
>
> **2026-11-24 下午 · 安装/接管/自启三段式（分支 `feat/install-uninstall-settings`）**：
> 注册表安装与接管开关彻底拆开。设置页常规页新增第 1 行「系统注册」：点击提权
> 安装/卸载 BHO；卸载走 `revolc-tray.exe --uninstall`，先退出所有 Revolc 进程，
> 再删 HKLM/HKCU BHO+CLSID 键、HKCU Run 和 `%APPDATA%\revolc\`，用户可以删目录走人。
> 第 2 行「接管（临时）」只写 disabled 标记，零 UAC、零注册表写；第 3 行仍是开机自启。
> 托盘改为永久显示，移除「显示托盘图标」行，旧配置的 `tray.enabled=false` 被忽略。
> 托盘启动时会检查 HKLM `InprocServer32`，绿色版移动目录后按需提权自修复。
> `SettingId::TrayEnabled` 删除；`WM_APP_REGISTRY_CHANGED` 用于工作线程完成后重绘设置页。
> 2026-11-25 补：切到「接管中」时托盘广播一次 `absorb_msg()`，让暂停期间已经打开的
> Explorer 文件夹窗口立即被 sweep 吸收，而不是等下一次随机扫描。

## P0 — 已知缺陷

1. **启动期吸收「已打开的窗口」是串行的，每个窗口约 100–150ms**
   （2026-11-18 实测 log：`hide → style → SetParent → framechanged` 每步 20–40ms，
   跨进程同步调用的固有代价）。4 个窗口 ≈ 0.5s，视觉上像“一个个被吞进去”。
   想法：吸收与摆位拆成两步（SetParent 用 `SWP_ASYNCWINDOWPOS` 之后不再同步等），
   或者按窗口 post 一个工作项、下一个窗口的 SetParent 与上一个的摆位重叠。
   非阻塞的替代：启动时先给所有待吸收窗口套一层遮罩（见 P2）。
2. ~~**`startup path` 不做校验**~~ **已修**（`src/main.rs::valid_startup_path` 用 `GetFileAttributesW`
   校验，非目录 / 不存在时回退 `home_dir()`；原 bug 是 `revolc.exe Explorer` 把字面量 `"Explorer"`
   当路径 spawn，log 实锤 `2026-11-17T00:40:30 path="Explorer"`）。
3. **soak 测试**（交接文档 §6-1）：日常使用 1–2 天，重点开关机/注销/强杀后无僵尸窗口，
   以及“连开 10 个文件夹是否每个都进 tab”。

## P1 — 里程碑主线

4. ~~M1 会话持久化~~：**缓办**（2026-11-18 决定先做 M3）。~~M2 书签栏~~：**已做**（2026-11-23 三期，见下方 4b）。
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
   **2026-11-26 补**：① 标签视觉留白 `TAB_TOP=4` 不变，但 `hit()` 命中从 y=0 起、
   顶部缩放条只对空白带生效，最大化时贴着窗口顶边也能点中标签（`fc66a2d`）；
   ② 主题新增 `tab_close_hover`，鼠标压在标签 X 上时画独立圆角暗底（`42606ad`）。
5b. **标签占满行为的两条支线（未做，先记下要改哪里）**：
   a) 单行缩窄 —— 现在已经是 `TAB_MIN_W=90` / `TAB_MAX_W=190` 的折中，要更彻底地缩窄得配合文字省略策略。
   b) 多行标签 —— 需要 **band 高度动态化**。目前当成常量 34 的地方：`ui/mod.rs` 的 `TABBAR_HEIGHT`
   （`view_rect`、`WM_NCHITTEST` 的条带判定、`invalidate_band`、吸收窗口的 rect.top）以及
   `ui/tabbar.rs` 里的 `TAB_BOTTOM` / `CENTER` / `hit()` 的 y 范围。改成 `TabBar::height()` 由
   `TabBar` 自己算（行数 × 行高 + 边距），其余地方都从它取即可。标签形状已经是"上圆下直"的独立区域，
   不依赖底下压着什么，多行不会露圆角。

6. ~~**M3 第二阶段：tab 拖拽分窗/合并**~~ **已做**（2026-11-26，分支
   `feat/tab-drag-detach`，commit `34c5007`，合并提交 `a23fa34`；`Rebuild_Target_Spec.md` §3.1）。
   多窗口地基（M3.2a，每进程一窗口）早已落地，本阶段在其上加了：
   - 分离带几何 `src/ui/drag_detach.rs`：左右 = 窗口边界，上下 = 标题栏 ±0.5 标题栏高（17px）
   - 拖出带 → 分层 popup 浮动（原槽位不画）；拖回带 → 恢复窗口内排序
   - 松手落桌面 → `detach_to_new_window` spawn 新进程，继承几何（`--geom` / `--maximized`）
   - 松手落别的窗口带 → `WM_COPYDATA` 交接，目标画虚化 ghost，close+reopen 接手
   - 设置页标签一视同仁可分离；`WM_CAPTURECHANGED` 取消不误提交
   剩余体验点：磁吸手感、边界细节（见 `docs/M3.3-Drag-Detach-References.md` 实施对照）。
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
8. ~~**DWebBrowserEvents2 订阅**~~ **已做**（三期 2.5，`5879be7`~`2dc9f91`，见上方 4b）；
   导航/标题毫秒级同步，200ms 扫描只作兜底。剩下的是验证 BHO 被安全软件禁用时的降级。
9. **REA 对照 Clover 的三个细节**：点击 tab 区域时的 SetForegroundWindow 顺序、
   Alt+Tab 表现、关闭语义；以及吸收后 `Ctrl+Z` / 右键菜单 / OLE 拖放是否正常
   （输入队列只解决键盘）。
10. **发布形态**：release DLL 签名、安装器、注册表开关设置页、
    BHO 被安全软件禁用时的降级提示。

## 参数速查（调参先看这里）

`src/ui/mod.rs`：`SWEEP_FAST_MS`(15) / `SWEEP_IDLE_MS`(200) / `BURST_WINDOW`(1.5s) /
`EXCLUDE_TTL`(8s) / `RESERVE_TTL`(6s)。
`src/shell/host.rs`：`PRE_SHOW_GRACE`(1.5s，等 Explorer 自己 ShowWindow 的宽限期)。
