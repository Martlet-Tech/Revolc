# Chrome 一体化 + 设置页改造计划

> 制定时间：2026-11-23 · 分支：`feat/unified-chrome-settings`
> 适用仓库：`Revolc/`（独立 git 仓库，基线 `master`）
> 目的：把三项改动固化成计划，避免实施中途方向偏移。每阶段结束附验收标准。

---

## 一、已定决策（不再讨论）

### 做

1. **收藏栏与标签栏一体化绘制**：合并为一条 62px 的 chrome 条，收藏栏不可隐藏。
2. **设置按钮放在收藏栏右端**：点击打开设置页。
3. **设置页以新标签页形式呈现**：不新开窗口，是主窗口里的一个 tab。

### 不做

- Everything 搜索栏（不下载、不接入、不做搜索结果页）。调查结论留作参考：Everything SDK 是 IPC 包装，必须后台运行 Everything.exe；官方无 headless 参数，MIT 许可允许分发。本计划不涉及。

### 抽象命名（已定）

按"内容由谁拥有"划分，不按具体页面名：

```
TabKind（概念分类，不落在 Tab struct 上）
├── Hosted   外部窗口被 re-parent（Explorer，将来可加记事本/计算器）
└── Painted  Revolc 自绘页面（无 HWND，主窗口给焦点）

PaintedPage（开放枚举）
├── Settings
├── Search    <- 将来 Everything 页加这里
└── ...
```

**kind 不放在 `Tab` struct 上**，放 `App.views` 的 `View` enum 里。`tabs/mod.rs` 保持纯逻辑无 Win32 依赖。

---

## 二、当前架构事实（调查得出，勿凭记忆改）

### 窗口结构

- 主窗口 `RevolcMain`：`WM_PAINT` 只画 `paint_band` + `paint_frame`，其余像素归被接管的 Explorer 子窗口（`WS_CLIPCHILDREN`）。
- **标签栏是独立 owned popup** `RevolcOwnedTabBar`，34px，`tabbar_hwnd`，`tabbar_wndproc` 把鼠标消息转发给主窗口。
- **收藏栏是另一个独立 owned popup** `RevolcOwnedBookmarkBar`，28px，`bookmarkbar_hwnd`，目前只画"收藏夹"占位文字。它转发鼠标消息给主窗口时**没有做坐标偏移**（已知 bug，一体化后自然消失）。
- 两个 popup 用 `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_POPUP | WS_CLIPSIBLINGS`，owner 是主窗口。关键特性：**owned popup 永远在 owner 的子窗口之上**，防止 Explorer 窗格盖住 chrome。改回主窗口自绘会破坏这个 Z 序保证。

### Tab 模型

- `tabs/mod.rs`：`Tab { id, path, label }` + `TabList`，纯逻辑，有单测。
- `App.views: Vec<Option<Absorbed>>` 与 `tabs` **按 index 严格平行**。`None` 当前语义是"tab 已建、Explorer 窗口还没到"（reserved）。
- 按 index 遍历/同步的地方（改动时必须逐处确认）：
  - `sweep`（1770）：`expecting = burst || any(v.is_none())`
  - `expire_reserved`（1545）：把 None 槽当超时 tab 删除
  - `drop_dead_views`（1620）
  - `enforce_views`（2119）：`let Some(v) = slot else { continue }`
  - `refresh_labels`（1662）、`resolve_pending_paths`（1678）
  - `activate`（1949）：`focus_view` + `sync_input_queue` 依赖 view
  - `close_tab`（1897）：None 槽 push 到 `discard`
  - `tab_by_label_tail`（1176）：`v.is_some()` 做过滤
  - `on_explorer_navigated`（1129）：按 label 匹配，pid 回退遍历 views
  - `move_tab`（1324）：views 与 laid_out 同步
- `laid_out: Option<(usize, ...)>` 按 active index 缓存几何。

### 配置

- `common/src/config.rs`：`Config { theme, behavior, takeover, tray, bookmarks }`，TOML 热重载（`ArcSwap` + notify 500ms debounce）。
- 用户实际配置是绿色系 ramp（`tabbar_background #1b3d28` 等），代码默认值仍是深灰蓝——默认值与事实不符，但本计划不改默认值。
- `behavior.startup_mode` / `startup_paths` / `hover_switch` 是死配置，UI 不暴露。
- `Config` 只 derive `Deserialize`，写回要用按行替换函数（保住用户中文注释）。

---

## 三、四阶段实施计划

### 阶段 A：View 抽象重构（地基）

**目标**：把 `Option<Absorbed>` 升级为能表达三种状态的 enum，为 Painted tab 腾出位置；行为零变化。

改动：

1. 新建 `src/ui/view.rs`：
   ```rust
   pub enum View {
       Pending,                    // 等 Explorer 窗口（原 None）
       Hosted(Absorbed),           // 已接管，走 sweep/place/focus
       Painted(PaintedPage),       // 自绘，无 HWND
   }
   pub enum PaintedPage { Settings }
   ```
   提供 `as_hosted()` / `is_pending()` / `is_painted()` 等小助手，减少遍历处的 match 噪音。
2. `App.views: Vec<View>`。
3. 逐处改遍历（见"当前架构事实"清单）：
   - `known_tops` / `sweep` 的 stray 检测：只收集 `Hosted`
   - `expire_reserved` / `drop_dead_views`：只处理 `Pending`
   - `refresh_labels` / `resolve_pending_paths` / `enforce_views`：只处理 `Hosted`
   - `activate`：`Hosted` 走原逻辑；`Painted` 跳过 focus/输入队列，把焦点给主窗口
   - `close_tab`：`Pending` push discard；`Hosted` post WM_CLOSE；`Painted` 只 remove
   - `tab_by_label_tail`：只匹配 `Hosted` 槽
   - `move_tab`：views 里三种状态都要跟着 move
   - `laid_out` 只对 `Hosted` 有意义，`Painted` 槽不参与几何缓存
4. `tabs/mod.rs` 不动（保持纯逻辑）。需要区分的地方通过 `views[i]` 判断，不给 `Tab` 加字段。
5. 单测：纯逻辑部分（View 助手方法）加测试；`cargo test --workspace` 全绿。

**验收**：`cargo clippy --workspace --all-targets` 无 error；现有功能（开标签、切标签、关标签、sweep 吸收）行为不变。

### 阶段 B：一体绘制 + 设置按钮入口

**目标**：两个 popup 合并成一个 62px chrome 条；收藏栏不可隐藏；右端设置齿轮可点。

改动：

1. 常量：`CHROME_HEIGHT = TABBAR_HEIGHT + BOOKMARK_HEIGHT`（62）。删除 `BOOKMARK_CLASS`、`bookmarkbar_hwnd` 及相关函数。
2. `tabbar.rs`：`paint()` 扩展到画两行——上 34px 标签条（原逻辑），下 28px 工具栏（收藏目录占位 + 右端设置齿轮）。高度参数、`fill_tab`、`fill_round` 的 y 坐标不动（都在上半部）。
3. `caption.rs`：新增 `Settings` 按钮 slot（可选，若设置齿轮画在工具栏右端则不占 caption 按钮位——见下）。
4. `mod.rs`：
   - `tabbar_wndproc` 的 popup 高度改为 `CHROME_HEIGHT`，`sync_tabbar_window` 同步高度
   - `paint_band` 的离屏缓冲高度改 `CHROME_HEIGHT`
   - `invalidate_band` / `refresh_band` 的 band 高度改 `CHROME_HEIGHT`
   - `view_rect().top` 固定 `CHROME_HEIGHT`
   - `WM_LBUTTONDOWN`：y 在 34..62 且命中右端设置按钮 → `open_settings_tab()`；其余工具栏点击暂不响应（收藏目录 Phase 3 再做）
   - `WM_APP_CONFIG_CHANGED`：删掉 bookmark popup 相关同步
   - `bookmark_height()` 删除或固定返回 28
5. 配色：工具栏底色读 `bookmark_bar_background`（默认 `#202020`，用户可在 toml 改成绿色系）；设置齿轮用 `tabbar_text` 那档浅绿。

**位置决策**：设置齿轮画在**工具栏行右端**（y 34..62），不占标题栏三按钮的位置。这样标题栏布局完全不变，收藏栏"右端空白"正好利用。

**验收**：视觉上一条 62px chrome 条；收藏栏开关失效（不再可隐藏）；点齿轮能触发打开设置 tab 的动作（哪怕阶段 C 前只是 log）。

### 阶段 C：设置页 tab 内容与交互

**目标**：设置页作为 `Painted(Settings)` tab 能显示、能操作、能写回配置。

改动：

1. 新建 `src/ui/settings.rs`：
   - `SettingsPanel` 状态：当前分类、各控件值（从 Config 读）
   - `paint(hdc, rect, cfg)`：左侧分类导航 + 右侧设置区，配色从 Config 绿色 ramp 取
   - `hit(x, y)` → `Hit::{Category(i), Toggle{key, value}, Choice{key, value}, None}`
   - 控件：checkbox / radio / 下拉，GDI 自绘（参考 `caption.rs` glyph 画法）
2. `src/ui/mod.rs`：
   - `open_settings_tab()`：若已有 `Painted(Settings)` tab 则 `activate` 过去；否则 `tabs.push("设置")` + `views.push(View::Painted(PaintedPage::Settings))` + `sync_tabbar` + `activate`
   - `activate` 的 Painted 分支：隐藏所有 Hosted view，焦点给主窗口，`redraw_all`
   - `WM_PAINT`：当 active tab 是 Painted 时，在 view_rect 区域画设置面板
   - `WM_LBUTTONDOWN` / `WM_MOUSEMOVE` / `WM_KEYDOWN`：Painted tab active 时把消息路由给设置面板
3. `common/src/config.rs`：新增 `write_config_value(path, section, key, value)` 按行替换函数，保留注释与排版。
4. 设置项范围（只暴露真实生效的）：
   - 主题色：`theme.*` 颜色字段（文本输入或色板，Phase 1 可做文本）
   - `tab_radius`
   - `bookmark_bar_background`
   - `takeover.mode`
   - `bookmarks.enabled`（注意：一体化后收藏栏不可隐藏，此项要么删掉要么改为只控制"显示书签条目"——阶段 C 再定）
   - 不暴露：`startup_mode` / `startup_paths` / `hover_switch`（死配置）、`tray.enabled`（需重启）
5. 写回链路：`SettingsPanel` 改值 → 按行写 TOML → watcher 500ms → `WM_APP_CONFIG_CHANGED` → 主窗口与设置面板同步刷新。

**验收**：点齿轮打开设置 tab；改主题色主窗口实时变绿；切回文件夹 tab 正常；设置 tab 单例；`Ctrl+W` 关掉设置 tab 回上一 tab。

### 阶段 D：收尾验证与文档更新

1. `cargo fmt --check`、`cargo clippy --workspace --all-targets`、`cargo test --workspace`
2. 手工冒烟：开关标签、拖拽排序、右键菜单、设置 tab 切换、配置热重载
3. 更新 `docs/TODO.md`、`README.md`
4. checkpoint commit

---

## 四、风险清单

| 风险 | 应对 |
|---|---|
| `Option<Absorbed>` → `View` 改动面大（约 12 处遍历） | 逐处改、逐处编译；`expire_reserved` / `tab_by_label_tail` 是语义陷阱，重点检查 |
| 一体绘制后 Explorer 子窗口盖住 chrome | 保持 owned popup 结构，不改为主窗口子窗口 |
| 设置 tab 的键盘焦点与 Explorer 输入队列冲突 | Painted 分支跳过 `sync_input_queue`，焦点给主窗口 |
| 设置页改配置导致主窗口重排、闪一下 | 沿用现有 `WM_APP_CONFIG_CHANGED` → `layout()` 链路，不新增 |
| TOML 写回吃掉中文注释 | 按行替换，不整体 serialize |
| 设置 tab 被 `expire_reserved` 当超时删除 | `expire_reserved` 只处理 `View::Pending` |
| "设置"被当文件夹名匹配导航事件 | `tab_by_label_tail` 只匹配 `View::Hosted` 槽 |

---

## 五、明确不涉及

- Everything 搜索栏、搜索后端下载/安装、IPC 查询
- 收藏目录的真实数据与交互（TODO Phase 3）
- 主题色默认值修改（当前代码默认是深灰蓝，用户实际用绿色 toml，本计划不改默认值）
- `tray.enabled`、`startup_mode`、`hover_switch` 的行为修复
- 安装器、BHO、注册表

---

## 六、阶段状态跟踪

| 阶段 | 状态 | 完成时间 | commit |
|---|---|---|---|
| 计划文档 + 分支 | ✅ 完成 | 2026-11-23 | `409471a` |
| A · View 抽象 | ✅ 完成 | 2026-11-23 | `ee3e1af` · 20 tests pass |
| B · 一体绘制 + 设置按钮 | ✅ 完成 | 2026-11-23 | `56d7dc0` · 20 tests pass |
| C · 设置页内容 | ⬜ 待开始 | | |
| D · 收尾验证 | ⬜ 待开始 | | |
