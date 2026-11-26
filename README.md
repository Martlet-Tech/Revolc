<p align="center">
  <img src="assets/icon.png" width="112" alt="Revolc">
</p>

<h1 align="center">Revolc</h1>

<p align="center">把 Windows 资源管理器变成多标签浏览器<br><sub>Clover 的原生重制版 · 不内嵌浏览器内核</sub></p>

<p align="center">
  <code>C&nbsp;l&nbsp;o&nbsp;v&nbsp;e&nbsp;r</code><br>
  <code>r&nbsp;e&nbsp;v&nbsp;o&nbsp;l&nbsp;C</code>
</p>

<p align="center">
  <img alt="platform" src="https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0f6cbd?style=flat-square">
  <img alt="language" src="https://img.shields.io/badge/Rust-native%20Win32-b7410e?style=flat-square">
  <img alt="status" src="https://img.shields.io/badge/status-daily%20usable-2ea44f?style=flat-square">
  <img alt="license" src="https://img.shields.io/badge/license-MPL%202.0-2ea44f?style=flat-square">
</p>

![Revolc 截图](assets/screenshot.png)

---

## 先说 Clover

如果你用过三叶草（Clover），应该懂那种感觉：用完之后再也回不去原生资源管理器了。

它做的事其实很朴素——把浏览器那套交互搬到文件管理上：一个窗口开多个标签、双击文件夹在新标签里打开、`Ctrl+W` 关掉、拖动标签换位置。没有花活，但每一处都恰好是天天要用的那一下。

可惜它早就停更了。后来的系统一个比一个难缠，装上去要么打不开，要么把 explorer 弄崩。

所以有了 Revolc：**把 Clover 那股劲儿重做一遍，但换成这个年代该有的实现方式。**

## 继承了 Clover 的什么

- 文件夹在标签里打开，不再糊一屏窗口
- `Ctrl+T` 新建、`Ctrl+W` 关闭、中键点掉、拖动标签重排
- 把标签拖出标题栏带 → 分离成新窗口；拖进另一个 Revolc 窗口的标题栏带 → 合并成标签（目标显示虚化落点）
- 窗口 X = 真关；托盘只是可选的快捷遥控器，不装也能用
- **接管的是真实的 Explorer 窗口**

最后一条是重点。Revolc 不是自绘一个"像资源管理器"的文件列表——你看到的 Ribbon、地址栏、右键菜单、预览窗格、详情列，以及系统里装的所有 shell 扩展，**全都是原生组件**。所以它不会慢，也不会有"这个右键菜单怎么少了一半功能"的尴尬。

## 改变了什么

|  | Clover | Revolc |
|---|---|---|
| 技术栈 | 内嵌一整份 Chromium（2011 年的） | Rust + 原生 Win32，没有浏览器内核 |
| 体积 | 几十 MB，外加一串子进程 | `revolc.exe`（每窗口一进程）+ 可选 `revolc-tray.exe` + 一个很小的 BHO DLL |
| 接管方式 | 注入进程、装 hook、子类化窗口 | BHO 只发一条消息，主程序自己把窗口搬进来 |
| 退出后 | 卸载 / 注销 | BHO 立即失效（写禁用标记，不再弹 UAC）；HKLM 注册保留，彻底清除见 `docs/registry/` |
| 标题栏 | Chromium 自绘 | 一体化标题栏，标签条本身就是标题栏 |

具体说三件事：

**不注入、不 hook、不改 Explorer 的代码。** BHO 的作用只有一个：在新窗口创建时喊一声。剩下的判断、搬运、排布，全部由 Revolc 自己完成。这条路线最慢的部分是调试，但换来的是崩溃面的数量级下降。

**吸收靠双通道。** 通知会丢，所以除了 BHO 的提示，还有一个自适应的后台扫描兜底（空闲时 5 次/秒，预期有窗口时 66 次/秒）。单次扫描是亚毫秒级的，正常使用察觉不到。

**主开关只有两个。** 设置页在收藏栏右端的齿轮按钮里：常规页是「接管：是/否」和「开机自启：是/否」，后者只有接管为“是”时可选；同页还有主题预设、标签圆角和窗口 X 的行为，但那三个是外观/行为项，不是模式开关。

- 接管=否时启动 Revolc 只打开设置页（控制台），不会造一个空的文件管理器窗口；`+` 和书签也不会 spawn 系统 Explorer。
- 点「接管=是」：首次弹 UAC 安装 BHO，设置页让位给一个真正的工作 tab；BHO 通知 + 后台扫描两条路径一起工作，并把已经开着的文件夹窗口立刻收进来。
- 点「接管=否」：弹 UAC 卸载并清理——删注册表、开机自启和 `%APPDATA%\revolc\`，之后可以直接删软件目录。这里没有“临时暂停”状态。
- 不开自启：每次重启后手动运行一次 `revolc.exe`；开了自启则开机自动工作。会话恢复（上次打开的标签）尚未实现，见下方已知问题。

托盘图标在 Revolc 运行时常驻显示，没有独立开关。高级用户也可以直接改 `%APPDATA%\revolc\revolc.toml`，改完即时生效。退出请走托盘菜单或 `revolc.exe --shutdown`，被接管的窗口会原样回到桌面。

## 现在能用到什么程度

可以日常用。已知的毛病，不藏着：

- 开关标签时原生窗口会闪一下 —— re-parent 架构的固有代价，Clover 当年同款
- **别用任务管理器强杀**。被接管的文件夹窗口是 Revolc 的子窗口，会一起没掉。请走托盘退出
- 多窗口通过多个 `revolc.exe` 进程共存（托盘有窗口列表）；tab 拖出标题栏可分离成新窗口，拖进另一个窗口的标题栏可合并
- 启动时恢复上次的标签还没做
- Windows 11 上的圆角、贴靠布局、多显示器 DPI 还在打磨

## 自己编译

需要 Rust + MSVC 工具链和 Windows SDK：

```bash
cargo build --release                       # 主程序（revolc.exe）
cargo build --release -p revolc-tray        # 可选托盘（revolc-tray.exe）
cargo build --release -p revolc-bho         # BHO（要单独编，注册才生效）
```

托盘负责 BHO 注册、窗口列表、前台记账；不启动它时，双击文件夹仍会被已有的
`revolc.exe` 窗口接管，只是没有托盘快捷入口。退出请走托盘「退出接管」或
`revolc.exe --shutdown` / `revolc-tray.exe --shutdown`。

注册方式、排错清单、这一路上踩过的坑，都写在 [`docs/交接文档.md`](docs/交接文档.md) 里——那份文档比我诚实。

## 欢迎搭把手

一个人写 shell 扩展很容易钻牛角尖，所以特别欢迎这几种帮忙：

- **日常用一阵，然后告诉我哪里别扭。** soak 测试比任何单元测试都值钱：反复开关机、注销、强杀之后有没有留下僵尸窗口
- **Windows 11 / 多显示器 / 高 DPI** 上的表现（我这边测不了所有组合）
- **tab 拖拽分窗/合并**的体验反馈——已实现（拖出标题栏分离成新窗口，拖进别的窗口标题栏合并，目标显示虚化落点），但磁吸手感、边界细节还想听真实使用意见
- 托盘菜单、设置界面、安装器

开 issue 就行，附上 `%APPDATA%\revolc\logs\` 里的日志会比截图有用得多。

## 一句免责

它接管的是系统 shell。装之前请读一遍文档，注册表记得备份。真出问题，`docs/registry/` 里有清干净的脚本。

## 许可证

[MPL-2.0](LICENSE)。一句话：**拿去用、塞进你自己的东西里都行，改了 Revolc 的代码把那几个文件还回来就行**。没有 CLA，提 PR 不需要签任何东西。

---

<p align="center"><sub>致敬 Clover。那个把文件管理器做对了的工具。<br>名字反着写，事也重做一遍。</sub></p>
