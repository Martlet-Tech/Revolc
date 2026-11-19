# Clover「双击文件列表空白 → 返回上一级」实现调研

> 用 REA / Ghidra 12.2 反编译 `clover.dll`（x86, chrome_dll.pdb）与
> `TabHelper64.dll`（x86-64, Explorer BHO）。**只调研，未改 Revolc 代码。**
> 日期：2026-11-19。

## 结论先行

1. 这个功能**不是 Windows 原生行为**，是 Clover 自己造的轮子。
   - 微软官方「Up One Level」史（Raymond Chen, MSDN gg502640）：Vista 起只有
     地址栏面包屑 / `Alt+Up` / `Backspace`，原生 Explorer 从没有「双击空白回上级」。
   - 微软商店至今有人把它当独立付费工具卖（Explorer DoubleClick Up、FolderStepUp），
     佐证系统不带此功能。
2. 实现**全在 BHO（TabHelper64/32.dll，跑在 Explorer 进程内）**，不在 clover.dll 主程序。
   - BHO 用 `SetWindowSubclass` 钩住文件列表视图（`SysListView32`，老系统；
     `DirectUIHWND`，Win8+），在 subclass 回调里截 `WM_LBUTTONDBLCLK`。
   - 判断该坐标**没落在任何 item 上**（"空白"）后，向父窗口
     `SHELLDLL_DefView` 发 `WM_COMMAND, 0xA022`（= FCIDM_TB_UPFOLDER 一档的
     "向上一级" 命令），由 Explorer 自己执行导航。
   - 功能开关来自 clover.dll 主进程写的一块共享内存
     `Local\CloverSetting_MappingObject`（对应设置页
     `browser.double_click_to_navigate_up`）。
3. 对 Revolc 的含义：**这是可以小成本复制的**，因为 Revolc 已经有 BHO
   （`bho/src/lib.rs`），架构位置和 Clover 完全一样。但要复制的是「BHO 里 subclass
   文件列表 + 判断空白 + 发 `WM_COMMAND 0xA022`」，不需要动主窗口、不需要碰导航实现。

## 证据链

### 01 功能开关是 Clover 自定义 pref，只走设置 UI

`clover.dll` 字符串 `browser.double_click_to_navigate_up` @ `0x1fb7908`，
全 DLL 仅 3 个函数引用：

| 函数 | 作用 |
|---|---|
| `FUN_01d5fb20` | 注册 pref 默认值，把 3 个鼠标开关写进 70 项配置数组 |
| `FUN_01dc77f0` | 设置页 UI：建复选框并绑定该 pref |
| `FUN_01dc9570` | toggle 回调：读/写回该 pref |

**没有任何行为函数直接引用这个字符串**——开关值被写进共享内存供 BHO 读取，
所以双击动作的执行端在 BHO。

### 02 BHO subclass 文件列表视图（决定性）

`TabHelper64.dll`：

- 导入 `COMCTL32.DLL::SetWindowSubclass / RemoveWindowSubclass / DefSubclassProc`、`USER32.DLL::GetDoubleClickTime`。
- `FUN_18000e250(HWND 顶层)`:
  ```c
  h = FindChild(top, L"SysListView32");        // 老 Explorer 文件列表
  if (!h) { h = FindChild(top, L"DirectUIHWND"); flag28 = 0; }  // Win8+
  else flag28 = 1;
  SetWindowSubclass(h, FUN_18000e080, 0, ctx); // 装回调
  ```

### 03 subclass 回调里截双击、判空白、发「向上一级」命令

`FUN_18000e080(ctx, msg, wp, lp, ...)`:

```c
if (msg == 0x203 /* WM_LBUTTONDBLCLK */) {
    LRESULT r = DefSubclassProc(hwnd, 0x203, wp, lp);   // 先给 Explorer 正常处理
    if (feature_enabled() && is_blank_point(ctx, pt)) {
        SendMessageW(parent /* SHELLDLL_DefView 的父 */, 0x111 /*WM_COMMAND*/, 0xA022, 0);
    }
    return r;
}
// 还自己用 GetTickCount + GetDoubleClickTime 判定 click 序列（0x201/0x208），
// 对 DirectUIHWND（无 LVHITTEST）走另一条路径 FUN_18000e030。
```

- `0xA022` 落在 `FCIDM_BROWSERFIRST (0xA000)` 一档，是 Explorer framework 的
  "Up one folder" 命令 ID（同档 `0xA001 = FCIDM_TB_UPFOLDER`）。
- `is_blank_point` = `FUN_18000de60`：
  - `SysListView32`：发 `LVM_HITTEST (0x1012)`，返回项索引 == -1 即空白；
  - `DirectUIHWND`：用 UIAutomation 取该点元素类名，比对 `UIItemsView` /
    `UIGroupItem` 判断是否是条目。
- 另有 `FUN_18000dd50`（`WM_MOUSEWHEEL` 路径）做类似判断后，通过
  `IShellBrowser::BrowseObject(pidl, 2 /* SBSP_NAVIGATE... */)` 导航——是配套的其他鼠标手势。

### 04 开关通过共享内存跨进程传递

`FUN_18000cb90`：
```c
h = OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, L"Local\\CloverSetting_MappingObject");
p = MapViewOfFile(h, ..., 0x400);
memcpy(&g_settings, p, 0x118);   // 与 clover.dll 写的 70 项(0x118B) 数组对齐
```
`FUN_18000e080` 里 `feature_enabled()` 就是查 `g_settings` 对应位（`DAT_18002f4cc`）。

## 对 Revolc 的落地建议（未实施）

### 该抄的部分（小）
- 在 `bho/src/lib.rs` 的 `SetSite` 里，除了报 HWND，额外找到文件列表窗口
  （`SysListView32` / `DirectUIHWND`）并 `SetWindowSubclass`。
  - 注意现有注释明确警告：在 `SetSite` 里对 site 对象做 `QI(IWebBrowser2)` 会崩，
    subclass 是针对子 HWND，不碰 site 对象，安全边界不同，但**安装时机要避开
    Explorer 的窗口创建路径**，宜延迟到收到 `WM_CREATE`/导航完成后装。
- subclass 回调里只做：`WM_LBUTTONDBLCLK` → `LVM_HITTEST` 判空白 → 发
  `WM_COMMAND 0xA022` 给父窗口（DefView）。导航交给 Explorer 自己。
- 开关可直接写死为「开」或接 Revolc 现有 config/共享机制；不必先做 pref UI。

### 值得注意的坑
- **Win11 文件列表是 `DirectUIHWND`，没有 `LVM_HITTEST`**。Clover 用 UIAutomation
  （`UIItemsView` / `UIGroupItem` 类名）判空白。这是最大的兼容成本：要么照抄 UIA 探测，
  要么换个更简单的等价判据（见下）。
- Revolc 把 Explorer 顶层窗口 re-parent 成主窗口的子窗口、并剥掉 caption/改样式，
  但 **DefView / 文件列表这一层没动**，`WM_COMMAND 0xA022` 发给 DefView 父窗口仍应有效。
  需要在接管态实测确认（沙箱无桌面，交给用户手测）。
- 命令 ID `0xA022` 是未文档化的 shell 命令，跨 Win10/11 是否一致要在目标系统上验证；
  更稳的替代是直接拿 `IWebBrowser2::GoUp()`？——实际上 `IWebBrowser2` **没有 GoUp**，
  只能 `BrowseObject` + PIDL 父，或模拟 `Alt+Up`。Clover 选 `WM_COMMAND` 是走 Explorer
  内部最短路径，值得照抄并做版本验证。

### 更省力的替代（"新造轮子" vs "抄"）
- 若 UIA 探测太重：可以在 Revolc 主进程侧对**被接管的 DefView** 用
  `SendMessage(hwnd, WM_LBUTTONDBLCLK...)` 之外的方式——但跨进程 subclass 在 Revolc
  架构里不允许（这正是它用 BHO 的原因）。所以**仍应走 BHO**。
- 判空白的最轻量做法：把点击坐标发给文件列表做 `LVM_HITTEST`；若目标是
  `DirectUIHWND` 再退回 UIA。与 Clover 一致。

## 一句话回答用户的「重写还是新造车轮」

- 对 Clover：它是**真·自造轮子**（BHO subclass + 未文档化 shell 命令），不是白捡系统功能。
- 对 Revolc：**不用重写大轮子**——机制已验证清楚，且 Revolc 已有 BHO 落点，只需要在
  BHO 里加一个文件列表 subclass + 空白判断 + 发一条 `WM_COMMAND`，主程序零改动。
  难点只有 Win11 `DirectUIHWND` 的空白判定（UIA），其余都是几十行级别。

## 证据索引（REA evidence id）

- clover.dll pref 字符串与 3 个引用函数：`ev_e04f1373…`(search_strings)、
  `ev_4ee0eeb1…`(batch_decompile: 01d5fb20 / 01dc9570 / 01dc77f0)
- TabHelper64.dll subclass 安装（`FUN_18000e250`, SysListView32/DirectUIHWND）：
  `ev_d7a64f5e…`
- subclass 回调 `FUN_18000e080`（WM_LBUTTONDBLCLK → WM_COMMAND 0xA022）：
  `ev_6f7f7de9…`
- 空白判断 `FUN_18000de60` / 开关读取 `FUN_18000cb90`（共享内存）/ wheel 路径
  `FUN_18000dd50`：`ev_84cf155d…`
- 外部佐证：MSDN gg502640（Up One Level 史）、Microsoft Store 两款第三方工具、Wine shlobj.h FCIDM 常量表。

---

## 实现记录（分支 `feat/dblclick-empty-go-parent`，2026-11-19）

按上述机制在 Revolc BHO 落地第一版（仅 Win10 / `SysListView32`）：

- 新增 `bho/src/dblclick.rs`：
  - `SetSite` 调 `dblclick::arm()` → 同步扫一次 + 注册 750ms 周期定时器兜底；
  - 扫描用 `EnumThreadWindows(GetCurrentThreadId())` 枚举本线程顶层窗口
    （**不碰 site 对象**，规避 Revolc 的 explorerframe 崩溃）。找到
    `CabinetWClass`/`ExploreWClass` → `SHELLDLL_DefView` → `SysListView32`，
    `SetWindowSubclass` 挂回调（`HashSet<HWND>` 去重，失败回滚可重试）；
  - 回调截 `WM_LBUTTONDBLCLK`，`LVM_HITTEST` 判空白（`iItem < 0`），向
    `ShellTabWindowClass`（无则 DefView）发 `WM_COMMAND 0xA022`；
  - `bho/Cargo.toml` 加 `Win32_System_Threading` + `Win32_UI_Controls`。
- 验证：`cargo test` 14/14、`cargo build --release -p revolc-bho` 全过；
  DLL 产物 `target/release/revolc_bho.dll`（14:41，154KB）。
- **部署要点**：旧 BHO DLL 已被运行中的 explorer.exe 映射，必须**重启 Explorer /
  重新登录**才会加载新 DLL；这是本功能测试的前置条件。
- **范围限制**：Win11 的 `DirectUIHWND` 无 `LVM_HITTEST`，本版未覆盖（会直接跳过、
  不触发、也不误触发）。用户机器为 Win10 19045，命中本版路径。
- 未提交、未推送，等用户手测。
