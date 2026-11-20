# 系统文件夹打开"接管"机制调查

> 背景：Clover 的核心体验之一是"系统任何方式打开文件夹，打开的都是 Clover"，
> 且可在设置里灵活开关。但原版从"接管"切回"不接管"时容易弄坏系统 Explorer，
> 甚至必须重启电脑。本报告调查机制并给出重建方案的安全红线。
>
> 结论先行：**默认方案 = 窗口重定向（零注册表）；注册表关联仅作为高级可选项，
> 且只动 HKCU、带备份与自修复**。禁止碰 HKCR 机器级键。

## 1. 原版 Clover 是怎么做的（基于逆向证据 + 二进制字符串核实）

> 2026-11-16 修订：初版只依赖 Design_Spec 结论层，本节已对原始二进制做字符串级核实。

### 1.1 注册表关联（确认存在，此前遗漏）

对 `clover.dll`（x86）做 UTF-16 字符串扫描发现关键簇：

```
HKCU
Software
Classes
\shell\open\command
open
```

`\shell\open\command` 在 .text 中有 **2 处代码引用**（VA 0x1d462b1 / 0x1d46359），
不是死字符串。结合两处独立旁证：

1. 修复教程（博客园/CSDN）的标准步骤是"删除 `HKCU\SOFTWARE\Classes\Folder\shell`"，
   修复方向正是 HKCU 用户级 Folder 关联——说明 Clover 写的就是这个键
2. 版本日志 3.4.6 的"Cancel Associated Clover Entry"（取消 Clover 关联入口）
   说明关联是可开关的正式功能

**结论：Clover 的"接管文件夹打开"主通道 = 用户级
`HKCU\Software\Classes\Folder\shell\open\command` 关联**（优先级高于 HKLM，无需
管理员权限）。部分损坏案例中 HKCR\Folder\shell\open\command 的
`DelegateExecute={11dbb47c-...}` 也被牵连破坏（修复教程要求重建该值）——
这可能来自 Clover 对 HKCR 的写入或用户修复过程中的误操作，未在二进制中找到
Clover 直接写 HKCR 的证据，存疑。

### 1.2 ExplorerWatcher（确认存在）

clover.dll 含字符串 `TabHelper(ExplorerWatcher)` 及提示"在 IE 设置中重新启用
TabHelper(ExplorerWatcher) 加载项"——TabHelper 是以浏览器辅助对象形式注册的
Explorer 窗口监视器（对应 Design_Spec §3.1 的 hook 注入层），负责抓取/吸收
已打开的 Explorer 窗口进 tab 条。这是 Clover 不稳定的另一来源（注入层）。

### 1.3 不能确认的部分

- `lsftldr.dll` / `sftldr_wow64.dll` 字符串与关联簇相邻，但经查证它们是
  **微软 App-V 软加载器**的系统文件名，是否与 Clover 的接管机制有关未确认
  （可能是残留/无关引用，也可能是借助 App-V loader 实现的委托启动，二进制中
  未找到直接调用证据）。
- `HKCU` / `Classes` 字符串无直接 VA 交叉引用（经 reloc 扫描），推断经由
  字符串表基址+偏移访问；写入键的完整路径拼装逻辑需要重新跑 Ghidra 伪代码
  （clover_megadump.txt 只有导入表地址索引，无函数体）才能 100% 复原。

### 1.4 Clover 切换"接管→不接管"搞坏系统的机制（旁证推断）

修复教程的"修复"动作 = 删 HKCU 键 + 重建 HKCR 的 DelegateExecute + **重启电脑**。
反推损坏路径：卸载/取消关联时键清理不完整（残留指向已删除的 clover.exe），
或 HKCR 的 DelegateExecute 被破坏，shell 关联缓存不刷新。这正是本重建项目
要在设计上根除的问题类别。

## 2. 社区常见方案对比

### 方案 A：机器级注册表劫持（危险，禁止采用）

改 `HKCR\Folder\shell\open\command`（合并视图，实为 HKLM）。原版值：

```
(默认)          = %SystemRoot%\Explorer.exe
DelegateExecute = {11dbb47c-a525-400b-9e80-a54615a090c0}   ← Explorer delegate COM
```

第三方管理器把 `(默认)` 改成自己的 exe。**弄坏系统的三种典型方式**：

1. 丢掉/破坏 `DelegateExecute` → Win10/11 的 shell 打开路径残缺，Explorer 异常
2. 卸载/移动程序后残留指向不存在路径 → 双击文件夹"找不到应用程序"
3. Win10/11 对 Folder 关联有缓存/校验，修改后常要重启 explorer 甚至重启电脑才一致

教育类文章（CSDN、博客园修复教程）的修复步骤全是在恢复这些键 —— 即社区经验
已经证明此路线事故频发。**重建项目不采用。**

### 方案 A'：用户级注册表关联（可用但高危，仅作为高级可选项）

写 `HKCU\Software\Classes\Folder\shell\open\command`，优先级高于 HKLM，等效 A 但：

- 无需管理员权限；撤销 = 删键，影响面仅当前用户
- 无 `DelegateExecute` 时可正常打开，但已知副作用：部分应用（Chrome/Electron 系）
  的"在文件夹中显示"可能变慢（FileExplorerInterceptor 项目实测报告）
- 杀软对这类键敏感（sdclt UAC bypass 曾滥用同键位）——写法必须克制、可解释

### 方案 B：WinEventHook 窗口重定向（推荐，默认方案）

参考开源实现 `abdonkov/FileExplorerInterceptor`：

- 常驻监听 `EVENT_OBJECT_CREATE`；发现新 Explorer 文件夹窗口 →
  通过 Shell API（IShellWindows）读出其路径 → 关掉该窗口 → 以该路径启动本程序
- **零注册表**：切换开关即时生效，不需要重启 explorer/电脑，永远不会弄坏系统
  ——即便本程序崩溃，系统 Explorer 原样工作
- 附带优势：连 `explorer.exe /select,xxx` 这类直接调用也能拦到（注册表路线做不到）
- 代价：接管瞬间有一次"窗口关闭→新窗口"的闪烁；需要本程序常驻一个监听线程
  （属于主程序自身，不违背"无常驻组件"——可随接管开关启停）
- 细节约束：只在"窗口内是文件夹视图"时拦截（"此电脑"、控制面板不拦），避免误伤

## 3. 关键场景："打开所在文件夹"（浏览器下载列表）——硬验收标准

> 用户提问：接管后，浏览器下载列表的"打开所在文件夹"是否会调起本程序？
> （QTTabBar 做不到，只有 Clover 做到了——这是 M6 的**硬验收标准**，
> 达不到即视为接管模块失败。）

### 3.1 机制分析

浏览器"打开所在文件夹"调用 `SHOpenFolderAndSelectItems`（shell32），内部直接
驱动 Explorer 进程开窗（等价 `explorer.exe /select,path`），**不走
`Folder\shell\open\command` 关联**。因此：

- 纯注册表关联路线（QTTabBar/Explorer++ 类）在此路径上永远拦截不到
- Clover 能拦截，靠的是 **TabHelper(ExplorerWatcher)**：全局监听 Explorer 窗口
  创建（clover.dll 字符串证据 + SetWinEventHook 调用点），新 Explorer 窗口出现
  即被吸收进 tab 条（re-parent `SHELLDLL_DefView`）。无论窗口由谁拉起
  ——浏览器、下载器、explorer /select——一律可拦

### 3.2 重建路径（不注入）

| 路径 | 原理 | 效果 | 风险 |
|---|---|---|---|
| B1 关旧开新 | WinEventHook 检测新 Explorer 窗口 → IShellWindows 读路径+选中项 → 关窗 → `revolc.exe <path>` 启动新实例 | 100-300ms 闪烁；选中态丢失（可用命令行传 selected path 弥补） | 低 |
| B2 窗口吸收 | 检测新窗口 → FindWindowEx 定位其 `SHELLDLL_DefView` 子窗口 → 跨进程 `SetParent` 挂入本程序 tab 宿主 | 与 Clover 同级无感体验，选中态保留 | 中：跨进程 SetParent 有焦点/DPI/消息泵怪癖，**必须先 spike** |

- 不需要 DLL 注入：跨进程 `SetParent` 本身不要求注入目标进程
  （Clover 的注入是为了更深的控制，非 re-parent 的必要条件）
- 顺序：B1 先行保证功能可用 → B2 spike 验证 → 通过则升级为无感吸收
- 拦截判定：仅当窗口含文件夹视图（`SHELLDLL_DefView` 存在）且路径非
  "此电脑/回收站/控制面板"类虚拟位置时才拦

### 3.3 M6 验收清单

1. Chrome/Edge 下载列表"打开所在文件夹" → Revolc 打开该目录，选中该文件
2. 资源管理器双击任意文件夹 → Revolc 打开
3. `explorer.exe /select,path` 直接调用 → Revolc 打开并选中
4. 开关关闭后，以上全部回归系统 Explorer 原样行为，无需重启
5. "此电脑"/控制面板/回收站不被拦截

## 4. 重建决策

| 项 | 决策 |
|---|---|
| 默认行为 | 不接管（干净 Explorer） |
| 接管模式 1（推荐默认开启项） | 方案 B 窗口重定向，进程内线程，可随时开关 |
| 接管模式 2（高级，默认关） | 方案 A' HKCU 关联——**即原版 Clover 实际采用的通道**，
  实现必须遵守 Takeover_Research.md §3 安全红线 |
| 配置 | `takeover.mode = "off" \| "redirect" \| "associate"` |
| 里程碑 | M6（M3 拖拽分离之后，因为它复用"带路径启动新实例"的机制） |

> 决策说明：既然原版 Clover 的 UX 主通道就是 HKCU 用户级关联，`associate`
> 模式能做到和 Clover 一样的"任何方式打开文件夹都是本程序"的完整体验
> （`redirect` 只覆盖 shell 调用，拦不住 `explorer.exe /select` 直接调用）；
> 但它的安全红线必须比 Clover 严格——Clover 的失效案例（残留键、HKCR
> DelegateExecute 被牵连、关联缓存不刷新）就是红线要堵的每一个洞。

### 方案 A' 安全红线（不满足则不许实现）

1. **只写 `HKCU\Software\Classes`，永远不碰 HKCR/HKLM**
2. 写入前把原值（含 DelegateExecute）备份到本程序配置；关闭接管时精确恢复
3. 启动自检：发现 HKCU 键存在但本程序二进制校验不匹配 → 自动清除并告警（自修复）
4. 卸载程序时必须清键；安装包卸载钩子包含此步骤
5. 改动后调 `SHChangeNotify(SHCNE_ASSOCCHANGED, ...)` 通知 shell 刷新，避免要求重启
6. command 值格式 `"...\revolc.exe" "%1"`，不删不动系统 DelegateExecute（该键是
   HKCU 自己的子键，写默认值即可，系统在 HKLM 的 delegate 不受影响）

### 已知不追求的行为

- 不拦"此电脑/回收站/控制面板"等非文件夹视图（与 FileExplorerInterceptor 一致）
- 不做原版 Clover 那种"把 Explorer 视图 re-parent 进 tab"的运行时抓取 —— 我们是
  独立外壳 + IExplorerBrowser，重定向只需"关旧窗、开新实例"，不需要注入
