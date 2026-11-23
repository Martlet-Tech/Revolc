# Clover 重建目标规格（Rust 版）

> 本文档定义重建目标的行为规格。原版逆向分析见 `clover/Clover_Design_Spec.md`（只作参考，不复制其代码）。
> 本文档刻意精简：未写到的细节由实现者按常识决定，不需要逐条询问。

## 1. 产品定义

一个带多标签页的 Windows 文件管理器外壳：每个标签内是**真实的 Explorer 窗口**（`SetParent` 吸收进主程序），保留资源管理器的全部原生行为（Ribbon、右键菜单、缩略图、拖拽、地址栏、shell 扩展）。

- 不注入、不 hook、不改 Explorer 的代码。通知通道是一个很小的 BHO
  （`revolc_bho.dll`）：Explorer 加载它后，新文件夹窗口创建时通过
  `WM_APP_ABSORB` 报告给 Revolc；此外有一个自适应后台扫描兜底（通知会丢）
- 组件：`revolc.exe`（每窗口一进程）+ 可选 `revolc-tray.exe`（托盘遥控器，
  不装也能用）+ 可选 BHO DLL（接管模式依赖；导航事件订阅由 tray 写的标记文件
  驱动，见 TODO.md §4b）。无浏览器内核、无常驻服务进程
- 路线变更记录（2026-11-20 对齐）：初版规格曾计划 IExplorerBrowser COM 嵌入
  且"不做运行时 re-parent"；实际实现走了**吸收真实 Explorer 窗口**的 B2 路线，
  Ribbon 等 UI 天然完整（原 §8 开放问题就此关闭）。本文以下各节已按此路线改写

## 2. 平台支持

| 目标 | 要求 |
|---|---|
| OS | Win7 SP1 起步，Win10/11 完整体验，旧系统允许降级 |
| 架构 | x64 + x86 双 target |
| 降级原则 | 特性缺失只影响视觉效果（圆角→直角、透明→纯色、暗色→亮色），不影响功能；用运行时特性检测（GetProcAddress），禁止按 OS 版本号字符串分支 |
| 兼容基础 | manifest 声明全部 supportedOS GUID；x86/x64 双构建在 CI 中编译通过 |

## 3. 核心行为规格

### 3.1 标签页（已实现，拖拽分窗除外）
- 新建 tab（Ctrl+T）：打开新 Explorer 视图，默认路径 = 当前激活 tab 的路径
- 关闭 tab（Ctrl+W / 中键）：销毁该视图；最后一个 tab 关闭时询问是否退出
- 切换：Ctrl+Tab / 点击；悬停自动切换（原版 HoverTabSelector 行为）为可关设置
- 标签右键菜单（已实现，分支 `feat/tab-context-menu`）：在新窗口打开 / 关闭 /
  关其他 / 关右侧 / 右侧新建；三期已加"收藏此标签页 / 取消收藏"（分支
  `feat/bookmarks-phase3`，见 §3.3）
- tab 上双击空白处 = 新建；tab 上滚轮 = 切换
- ⬜ 拖拽分窗/合并：见 M3 第二阶段（§6），落地行为不变（落其他窗口 = 合并，
  落桌面 = 新进程承接）；前提是先把"多窗口"从 `App` 里拆出来

### 3.2 会话持久化（⬜ 缓办，2026-11-18 决定先做 M3）
> 目标行为不变，待排期。当前代码无会话保存/崩溃恢复；`WM_ENDSESSION`
> 时会把被吸收的窗口原样放回桌面（防悬挂引用），但这是退出清理而非持久化。
- 正常退出：保存全部窗口的 tabs（路径、激活态、窗口几何）
- 崩溃恢复：启动时检测上次非正常退出标志，提示"恢复上次会话"
- 启动行为设置项：恢复上次会话 / 打开新 tab / 打开固定路径列表（对应原版 `session.restore_on_startup` 三态）

### 3.3 书签栏（✅ 三期已实现：扁平书签 + 工具栏芯片；层级后置）
- tab 条下方的工具栏**与标签栏一体绘制**：同为 62px chrome 条的一部分
  （owned popup `RevolcOwnedTabBar`，`ui/chrome.rs` 画工具栏行），**不可隐藏**。
  2026-11-23 前的"独立 owned popup `[bookmarks].enabled` 开关"已废除，见
  `docs/Chrome一体化与设置页改造计划.md`；工具栏右端是设置入口（六边形图标）
- 工具栏左起画真实书签芯片（92px 固定宽、按窗口宽度决定可见数、长标签省略号）；
  点芯片在该窗口新开 tab；**右键芯片**弹「打开此收藏 / 取消收藏」，直接从收藏栏
  管理；**拖拽芯片**在行内水平重排，与标签栏共用 `src/ui/reorder.rs` 的中心线
  落点逻辑（窗口内，新顺序即时写回 `bookmarks.json`）。无书签时显示"右键收藏"提示
- 书签 = 文件系统路径（`Bookmark { label, path }` 扁平列表）；标签右键
  「收藏此标签页 / 取消收藏」用实时 BHO 路径，Painted（设置）页不提供。
  **文件夹层级后置**（四期之后）；设置页活动时新建 tab 回退 home 而非把
  "设置"当路径 spawn
- 保存格式：JSON（schema 带 `version=1` 便于迁移），文件名 `bookmarks.json`，
  与 `revolc.toml` 同在 `%APPDATA%\revolc\`。路径按大小写/斜杠/尾分隔符归一化去重
- 原子替换：`.tmp` → `sync_all` → `MoveFileExW(REPLACE_EXISTING)`；损坏/未来版本
  的 bookmark 文件回退空列表并记日志，绝不 panic（书签不能拖垮文件管理器）
- 所有配置/会话/书签文件写入必须原子替换（临时文件→flush→rename）
- 导航事件来源（已实现）：BHO 订阅 `DWebBrowserEvents2`，`WM_COPYDATA` 推送，
  tab 按 label（窗口标题）匹配——HWND/pid 匹配不可靠，见 TODO.md §4b 踩坑记录

### 3.4 窗口与实例（⬜ 现状：单窗口单进程；多窗口是 M3 第二阶段前置）
- 目标：单窗口多 tab、**多窗口并存**（典型用法：不同项目各开一个窗口、各一堆 tab）
- 支持多实例运行：实例通常由 tab 拖拽分离产生（见 §3.1），也可直接多次启动程序；
  各实例独立进程、独立会话、互不干涉
- 实例间无父子关系：拖出去就不回头，关闭顺序无所谓，会话各自保存
- ⬜ 待做：先把「多窗口」从 `App` 拆出（当前 `App` 持有唯一 `main_hwnd` + 一份 tabs）

### 3.5 快捷键
- 所有快捷键集中在一个统一的键位表中定义（按键 → 动作名映射），功能代码只订阅动作名，**禁止在功能实现里散落硬编码按键判断**
- MVP 不做键位绑定设置页，但改配置文件中的键位表即可生效
- 默认键位沿用 §3.1 所列（Ctrl+T/Ctrl+W/Ctrl+Tab 等）

### 3.6 设置页（✅ 2026-11-23 实现）
- 入口：工具栏右端六边形按钮 → 打开一个 **Painted 标签页**（单例：已存在则激活）
- 形态：`View::Painted(PaintedPage::Settings)`——无 HWND、由主窗口 GDI 自绘，
  与 `Hosted`（被接管的 Explorer 窗口）并列；抽象见 `ui/view.rs`
- 分类：常规（接管模式 off/redirect、标签圆角）/ 主题（绿 ↔ 深灰蓝预设、圆角）/ 关于
- 写回：`config::set_toml_values` 按行替换，保留用户注释与排版；一次写入触发一次
  watcher reload，主窗口与设置页同步刷新（**无重启**）
- 不暴露死配置：`startup_mode` / `startup_paths` / `hover_switch`（代码从不读取）
- 输入路由按**垂直分区**：y < 62（chrome 带：标签、caption 按钮）永远归窗口；
  只有 view_rect（y ≥ 62）才归设置页——见 `ui/chrome.rs::route_click`
  （踩坑：按"当前页面类型"一刀切曾导致设置页活动时窗口 X 失灵、双击误最大化）

## 4. UI 规格

### 4.0 配置文件
- 用户可自定义的东西全部收敛到**一份配置文件**（TOML，带注释示例）：主题配色、快捷键键位表、启动行为、悬停切换开关等
- **运行时可写、改动立即生效**（文件 watcher 监听 + 热重载），不是"退出程序才生效"或"程序锁住文件"的模式
- 程序自己写配置（设置变更、窗口几何）时也写同一文件；外部手改同样被接受，冲突以后写者为准
- 缺省值内置，配置文件可以从零开始只有两行也能跑

### 4.1 主题
- **运行时可改配色**：改配置文件或设置页均可，立即生效、无需重启
- 主题项至少包含：tab 条背景/前景、tab 激活/非激活色、工具栏配色、字体
- 内置两套预设（绿色 / 深灰蓝）+ 用户自定义；设置页「主题」分类里一键切换，
  切换写完整 11 个主题键、一次落盘（见 `ui/settings.rs` 的 `GREEN_THEME`/`DARK_THEME`）

### 4.2 标签页视觉
- 支持透明通道 + 圆角；实现层用分层窗口/自绘，Win7 等不支持的环境自动回退直角纯色
- 圆角半径、tab 高度为主题项

### 4.2.1 Chrome 式一体化标题栏（用户明确要求，高优先）
- tab 条与窗口按钮（最小化 `_` / 最大化 `□` / 关闭 `X`）在同一行，浏览器风格
- **实现于 M3 第一阶段**（2026-11-18，分支 `m3-integrated-titlebar`）：
  `WS_POPUP|WS_THICKFRAME|WS_MIN/MAXBOX|WS_SYSMENU` + `WM_NCCALCSIZE` 返回 0，
  tab 条与按钮由一个 **owned popup** 绘制（`RevolcOwnedTabBar`，`ui/tabbar.rs` +
  `ui/caption.rs`）。注意：早期注释说"主窗口自绘"，实际代码是独立 popup——
  owned popup 的 Z 序保证 Explorer 子窗格盖不住标题栏。
  `WM_NCHITTEST` 由主窗口回答 `HTCAPTION`（空白条带）与缩放边框；三个标题栏按钮
  **报 `HTCLIENT` 自己处理点击**（上报成 `HTMIN/MAX/CLOSE` 会让 Win11 Snap Layouts
  浮层错位、吃掉点击、留白块，2026-11-18 实测）。
- **2026-11-23 扩展**：工具栏并入同一 popup，`ui/chrome.rs` 画 28px 工具栏行，
  整体 62px；右端加设置按钮（六边形 glyph）。见改造计划文档

### 4.3 布局
- 62px chrome 条（34px tab 行 + 28px 工具栏行）+ Shell 视图填充剩余区域；
  `view_rect.top` 固定为 `CHROME_HEIGHT`（工具栏不可隐藏）
- 窗口几何记忆（位置/大小/最大化状态）

## 5. 日志与故障排查

- `tracing` + `tracing-appender`：结构化、文件轮转、组件级过滤（如 `com`、`tabs`、`persist`）
- release 默认 `info` 级，关闭的级别在编译期裁剪，零性能损耗
- 崩溃处理：minidump + 启动时把上次崩溃摘要写入日志头部
- 规范：错误路径必有日志；禁止调试期临时 log 混入提交；禁止 `unwrap()` 出现在 release 代码路径（CI 检查）

## 6. 工程

- 语言：Rust（stable），`windows` crate 作为 Win32/COM 绑定
- UI：纯 Win32 自绘（不用 egui/WebView，理由：需与 Shell COM 子窗口混合且要求 alpha 圆角自绘能力）
- 配置目录：`%APPDATA%\<App>\`（配置、日志、崩溃转储分目录）
- 里程碑（按序交付，每步可运行；状态截至 2026-11-20）：
  - ✅ M0：主窗口 + tab 条 + Shell 视图吸收 + 配置文件热重载骨架
  - ⬜ M1：会话持久化 + 崩溃恢复（缓办，见 §3.2）
  - ⬜ M2：书签栏（2026-11-23 三期落地扁平书签 + 工具栏芯片 + `bookmarks.json`
    持久化，见 §3.3；文件夹层级/pin 仍缓办）
  - M3：一体化标题栏（✅ 第一阶段，分支 `m3-integrated-titlebar`）+
    ⬜ tab 拖拽分窗/合并（第二阶段，见 §3.1）
    附带完成：标签右键菜单一期、BHO 导航事件推送（TODO.md §4b）
  - ⬜ M4：主题系统（含圆角/透明与降级）。部分完成：2026-11-23 设置页已实现
    （主题预设/圆角/接管模式切换 + TOML 按行写回 + 热生效），见 §4.1 与 §3.6
  - ⬜ M5：安装包（x64+x86）+ 更新检查
  - ⬜ M6：文件夹打开接管（§7）——BHO 接管机制已随日常使用落地，
    M6 剩余工作：`associate` 模式、安全软件误报的降级提示
    （接管开关设置页已随 2026-11-23 设置页落地：常规分类里切换 off/redirect）

## 7. 系统文件夹打开接管（机制已落地为 BHO；调查见 docs/Takeover_Research.md）

> 目标：复刻 Clover 的核心体验——"系统任何方式打开文件夹，打开的都是本程序"，
> 且开关完全可控、**绝不允许弄坏系统 Explorer 或要求重启**。
> 实现路线已从初版规格的 WinEventHook 监听改为 **BHO 通知**（见 §1）。

### 7.1 设置三态（配置项 `takeover.mode`）

- `off`（默认）：BHO 写禁用标记（`%APPDATA%\revolc\disabled`，SetSite 时检查），
  Explorer 原样工作
- `redirect`（已实现）：BHO 发现新 Explorer 文件夹窗口 → `WM_APP_ABSORB` 通知
  Revolc → 主程序吸收该窗口为 tab；另有自适应扫描兜底（BHO 未加载/通知丢失时）
- `associate`（⬜ 未做）：用户级注册表关联（`HKCU\Software\Classes\Folder\shell\open\command`）。
  高级选项，默认关闭，实现必须遵守 Takeover_Research.md §3 的六条安全红线
  （只动 HKCU、备份原值、启动自修复、卸载清键、SHChangeNotify 刷新、格式规范）

### 7.2 行为约束（已按 BHO 路线改写）

- BHO 只"喊一声"（发一条消息）：不注入、不 hook、不子类化 Explorer 代码
- BHO 注册在 HKLM（Explorer 只读 HKLM 的 BHO 列表，Clover 逆向佐证）；
  HKLM 缺失时降级 HKCU 并经 UAC 用 regsvr32 重试一次；禁用走标记文件而非删
  HKLM 键（避免每次开关都弹 UAC），彻底清除见 `docs/registry/`
- 只吸收真正的文件夹视图窗口；"此电脑/回收站/控制面板"等不拦
- 接管开关切换不要求重启 explorer 或电脑；BHO 失效时扫描兜底继续工作
- 本程序崩溃/退出时，系统 Explorer 的任何行为不受影响（红线：接管模块的可失败性
  必须只表现为"不再接管"，绝不表现为"系统打不开文件夹"）；
  `WM_ENDSESSION` 先把吸收的窗口放回桌面再退出

### 7.3 硬验收标准（M6）

浏览器"打开所在文件夹"（`SHOpenFolderAndSelectItems`，不走文件夹关联）必须
调起本程序并选中文件——QTTabBar 类纯关联方案做不到，这是与 Clover 对齐的
核心指标。BHO 通知路线天然覆盖此场景。

## 8. 已解决：Explorer 工具栏（文件/主页/共享/查看 ribbon）

初版曾担心 IExplorerBrowser 嵌入不含 ribbon。实际走了吸收真实 Explorer 窗口的
路线（§1），Ribbon、地址栏、预览窗格、详情列、全部 shell 扩展天然在内，
此问题关闭，不再需要 §4 前的结论。

## 9. 明确不做

- hook / 修改 Explorer 的代码（BHO 只是 Explorer 加载的合法通知器，发消息即止）
- 浏览器内核 / 网页渲染
- 遥测上传（启动时可选的版本检查除外，且可关闭）
- 替换系统目录树/面包屑（原版 Clover 的做法，已放弃；目录树/面包屑属于被吸收
  的真 Explorer 窗口本身）
- 机器级注册表劫持之外的破坏性行为（Folder 关联将来只动 HKCU，见 §7.1）
