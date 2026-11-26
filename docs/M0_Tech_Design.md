# Revolc M0 技术方案（历史归档）

> **历史快照（2026-11-16）**：M0 早期设计。核心路线后来已变——当时写的"单 crate、
> 不做 workspace"（现为 4 成员 workspace）、"IExplorerBrowser 每 tab 一个实例"
> （已废弃，改为 re-parent 真实 Explorer 窗口）、"tab 条自绘子窗口"（已改为
> 一体化标题栏 + owned popup）均不再适用。保留作为决策记录，现状以
> `docs/Rebuild_Target_Spec.md` 和 `docs/交接文档.md` 为准。§2.1 的评审决议仍有参考价值。

> M0 = 主窗口 + tab 条 + Shell 视图嵌入 + 配置热重载骨架。
> 本文档只管架构层；行为细节以 `Rebuild_Target_Spec.md` 为准。方案未覆盖的行为按常识实现，不逐条询问。

## 1. Crate 与模块划分

单 crate（`revolc`），M0 不做 workspace 拆分：

```
src/
  main.rs          入口：解析命令行（初始路径）、初始化日志/配置、进入消息循环
  app.rs           应用状态聚合（配置句柄、窗口列表、单实例无——见规格§3.4 多实例）
  config/          配置加载 + 文件 watcher 热重载（notify crate）
    mod.rs
  ui/
    mod.rs         主窗口（Win32 注册/创建/消息循环）
    tabbar.rs      tab 条自绘控件（子窗口，WM_PAINT 自绘；M0 先不要求 alpha 圆角）
  shell/
    mod.rs         COM 初始化（STA）与安全封装
    explorer.rs    IExplorerBrowser 封装：每 tab 一个实例，宿主子 HWND
  tabs/
    mod.rs         Tab/TabList 模型（纯逻辑，可单测，不依赖 Win32）
    events.rs      动作（Action）定义 + 键位表（Keymap）解析与匹配
  logging.rs       tracing 初始化（console+文件轮转，release 编译期裁剪）
```

铁律：`tabs/` 模型层不 import `windows` crate（保证可单测）；COM 调用只出现在 `shell/`；绘制只出现在 `ui/`。

## 2. 线程模型（最易错，固定不变）

```
主线程（STA）
 ├─ Win32 消息循环（GetMessage/TranslateMessage/DispatchMessage）
 ├─ 全部 UI 绘制（tab 条）
 ├─ 全部 COM Shell 对象（IExplorerBrowser 必须在创建它的 STA 上使用）
 └─ 键位匹配 → 发出 Action → 直接在同线程处理（无锁）

辅助线程（仅两个，M0 都由 std/tokio-current-thread 风格裸 std::thread 实现）
 ├─ config-watcher：notify 事件 → 原子交换 Arc<Config> → PostMessage 通知主线程重绘
 └─ 无其他线程。禁止在非主线程触碰 HWND/COM。
```

- 配置对象：`ArcSwap<Config>` 或 `RwLock<Arc<Config>>`，主线程每帧/每消息取快照，无长持锁
- tab 模型：主线程独占，无锁（M0/M1 阶段不需要跨线程共享）

## 2.1 设计评审决议（2026-11-16，接手评审）

1. **键位检测必须事件驱动**：键位匹配在 WM_KEYDOWN / TranslateAccelerator 时进行
   （`tabs/events.rs` 保持纯逻辑，接收 (vk, modifiers) 匹配）。
   **禁止 `GetAsyncKeyState` 轮询**——会漏键，且与 Shell 子窗口抢焦点时行为不可控。
   键位表（Binding/Action/Keymap）结构不变，只换检测方式。
2. **`ExplorerView` 禁止 `unsafe impl Send`**：IExplorerBrowser 是 STA 对象，类型上
   就要锁死"只在主线程使用"，防止未来跨线程搬家变成静默 UB。
3. `windows` crate 0.58 实测要点：CLSID 是 `Shell::ExplorerBrowser`（GUID 常量）；
   视图模式走 `SetFolderSettings(FOLDERSETTINGS{ ViewMode: FVM_DETAILS.0, fFlags: 0 })`；
   `IOleWindow`（取宿主 HWND）在 `Win32::System::Ole`，需开 `Win32_System_Ole` feature。

## 3. M0 技术验证点（先 spike 后铺开）

按序验证，任一 ✗ 则停下调整方案再继续：

1. **S1** `IExplorerBrowser` 创建 → `SetSite`/`CreateAndInit` 进子 HWND → 浏览到 `C:\` 可见可操作
2. **S2** 三个 tab = 三个子 HWND，切换 tab 靠显隐（ShowWindow），焦点归还正确
3. **S3** tab 条自绘子窗口：绘制、点击选中、WM_SIZE 布局联动
4. **S4** 配置 watcher：改 TOML → 主线程收到通知 → tab 条颜色变化，全程不卡 UI

## 4. 依赖清单（M0）

| crate | 用途 |
|---|---|
| `windows` (官方, 版本锁定) | Win32/UI/Shell COM；features 只开用到的 |
| `notify` | 配置文件 watcher |
| `tracing` + `tracing-appender` + `tracing-subscriber` | 日志 |
| `serde` + `toml` | 配置 |
| `arc-swap` | 配置快照 |

明确不用：tauri/egui/winit/任何浏览器内核。

## 5. 命令行与多实例

- `revolc.exe [path]`：带路径启动 = 初始 tab 打开该路径；无参数 = 用户目录（Win7 兼容：`SHGetKnownFolderPath` 不可用则回退 `USERPROFILE`）
- 拖拽分离起新实例的传参机制属于 M3，M0 只需保证"带路径启动"成立

## 6. 日志规范落点

- 初始化顺序：日志先于一切（含 COM init），保证启动失败可查
- 每个里程碑结束时输出一条 `info!("milestone=M0 booted in ?ms")` 式的启动摘要
