# Revolc UI 规格（强制约束）

> 新增控件、页面、主题或交互前必须先读本文件。
> 真相源：`theme/` crate 的语义键定义 + 本文件的映射表。
> 用词：**必须** = 不这样做不合并；**禁止** = 一律不允许；**建议** = 默认遵循，例外需说明理由。
> 详细背景与踩坑记录见 `交接文档.md`、`Chrome一体化与设置页改造计划.md`；本文件只写约束。

---

## 1. 三条铁律

1. **必须**从 `Theme`（语义令牌）取色；**禁止**在 UI 代码出现 `#RRGGBB` 或 `COLORREF(0x...)` 字面量，托盘进程同样适用。
2. **必须**按 §3“组件 → 令牌映射”实现控件；映射表里没有的控件，**必须**先在此表补一行再写代码。
3. **必须**为可点击元素实现 hover / disabled / focus 态；**禁止**出现“只能靠文字说明才知道能点”的控件。

---

## 2. 令牌契约（由 `theme` crate 提供）

主题文件只允许提供下列键；**禁止**主题文件发明组件专用键（如 `close_button_color`）。

| 键 | 语义 | 约束 |
|---|---|---|
| `app.background` | 最深底色：窗口边框、设置页画布 | 与 `chrome.background` 同色系、更深一档 |
| `chrome.background` | 标签条（34px 带） | 标签与三按钮都落在它上面 |
| `surface.background` | 书签栏、侧栏、托盘菜单底 | 与 chrome 同色系，非中性灰 |
| `surface.raised` | 卡片、菜单分组底、选中项 | 比 `surface.background` 亮一档 |
| `surface.hover` | 行 / 按钮 / 芯片 hover | 与 raised 可同值，但必须可区分于默认态 |
| `border.subtle` | 1px 描边、分隔线 | 浅色主题用深一档，深色主题用浅一档 |
| `foreground.primary` | 标题、正文、活动标签文字 | 与所在底色对比 ≥ 4.5:1 |
| `foreground.secondary` | 说明、非活动文字、图标 | 可读但弱于 primary |
| `foreground.inverse` | 浅色填充上的文字（如浅绿活动标签） | 只用于 `accent.soft` 之上 |
| `accent.default` | 强调：活动指示条、开关 on、焦点环 | 全主题唯一强调色 |
| `accent.soft` | 浅色强调面：深色主题的活动标签 | 浅色主题下不得当大面积底色 |
| `danger.default` | 关闭 hover、退出项 | 全应用只有一个危险色，托盘共用 |
| `danger.hover` | 危险态按下/深 hover | 可选；缺省时由 default 推导 |
| `focus.ring` | 键盘焦点 | 可选；缺省时由 accent 推导 |

主题文件还必须有：`name`（显示名）、`type`（`light` | `dark`）、可选 `inherit`（父主题名）。
`type` 决定对比方向和活动标签的映射分支，不允许靠肉眼猜。

---

## 3. 组件 → 令牌映射（强制）

| 组件 / 状态 | 取色 |
|---|---|
| 标签条底 | `chrome.background` |
| 标签 · 活动（dark 主题） | `accent.soft` 填充 + `foreground.inverse` 文字 + 2px `accent.default` 下边线 |
| 标签 · 活动（light 主题） | `surface.raised` 填充 + `foreground.primary` 文字 + 2px `accent.default` 下边线 |
| 标签 · 非活动 / hover / 关闭 hover | `surface.background` / `surface.hover` / `danger.default` |
| 书签栏底 / 书签芯片 | `surface.background` / `border.subtle` 描边 + `foreground.secondary`；hover 用 `surface.hover` |
| 标题栏按钮 hover / 关闭 hover | `surface.hover` / `danger.default` |
| 设置页画布 / 左导航 / 导航选中 | `app.background` / `surface.background` / `surface.raised` + 3px `accent.default` 左竖条 |
| 设置卡片 / 行 hover / 禁用态 | `surface.raised` + 1px `border.subtle` / `surface.hover` / primary 与底色等比混合 |
| 开关 on / off、分段选中、色卡 | `accent.default` / `surface.hover` / `accent.soft` + `accent.default` |
| 托盘菜单底 / 分隔线 / 行 hover | `surface.background` / `border.subtle` / `surface.hover` |
| 托盘关闭列 hover / 退出项 | `danger.default` + `foreground.inverse` / `danger.default` 文字 |
| 新控件（本表未列） | 先补表：它属于哪一层面（chrome / surface / raised）、什么状态、强调还是危险 |

---

## 4. 排印与空间

- 三种字号，**禁止**更小的字：Title 16px/500、Body 13px/400、Caption 11px/400。
- 中文用 `Microsoft YaHei UI`，ClearType；英文与数字沿用 `Segoe UI`。字体创建走统一 helper，**禁止**散用 `DEFAULT_GUI_FONT`。
- 4px 栅格：4 / 8 / 12 / 16 / 24 / 32。卡片内边距 12、卡片间距 12、内容区最大宽 720。
- 圆角：控件 4、卡片与标签 6、菜单与 popup 8。
- 所有 px 常量都是逻辑像素，绘制前乘 `GetDpiForWindow(hwnd) / 96`；100/125/150% 三档必须截图验收。

---

## 5. 结构约束

- chrome 始终是 owned popup，层级高于被接管的 Explorer 子窗口；**禁止**改回主窗口子窗口。
- 设置页的 `paint()` 与 `hit()` **必须**共用同一个 `layout()` 结果；**禁止**在命中检测里复算布局。
- 托盘菜单**必须**分组为“窗口 (N)”与“操作”，**禁止**显示 `1#:` 这类内部序号。
- 输入路由按**空间区域**分段（chrome 带 / view rect），**禁止**按当前页面类型一刀切吞掉消息。
- 视图模型的 `Pending / Hosted / Painted` 语义不得混淆：`expire_reserved` 只处理 Pending，`tab_by_label_tail` 只匹配 Hosted。

---

## 6. 主题扩展

- 主题是 JSON 文件，只提供 §2 的键；新增内置主题 = 新增一个 JSON + 一个测试，**不改 UI 代码**。
- 内置四套：`普通浅色`、`普通深色`、`三叶草浅色`、`三叶草深色`。
- 用户主题放 `%APPDATA%\revolc\themes\`；缺失键按 `inherit` → 内置主题 → 按 `type` 推导的顺序补齐。
- 从 VS Code 等外部主题导入时，只映射到 §2 的语义键；映射表由 `theme` crate 维护，UI 不参与。
- 旧 TOML 里的扁平色键只作为一次性迁移的 overrides 读入，**禁止**长期并存两套色值真相源。

---

## 7. 验收清单

- [ ] UI 代码中 grep 不到孤立色值（`#RRGGBB` / `COLORREF(0x…)`）
- [ ] 新增控件在 §3 有对应映射行，hover / disabled / focus 齐全
- [ ] 四套内置主题 + 一套用户自定义主题在三个界面切换无残留旧色
- [ ] 100% / 125% / 150% DPI 三档无截断、无错位
- [ ] 托盘与主窗口危险色一致；设置页无“点击才能发现”的裸文本控件
- [ ] `cargo test --workspace` 全绿，`cargo clippy --workspace --all-targets` 无 error
