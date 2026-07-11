# AutoKey-Rust

AutoKey-Rust 是一个 Windows 桌面按键调度工具，使用 Rust、egui 和标准 Win32 API 实现。

## 功能

- 12 个独立按键槽，可点击捕获普通键、数字键、方向键、小键盘键和 F1-F24
- 每个按键可单独启用，并配置基础间隔和随机浮动
- 独立循环与顺序循环
- 手动停止或设置最大循环次数
- 前台 `SendInput` 和绑定窗口 `PostMessage` 两种发送方式
- 左 Alt 单独按下切换启动/停止
- `Ctrl+Alt+Space` 绑定鼠标所在窗口
- 按住鼠标右键拖动超过 8 像素后绑定目标窗口
- 多配置保存、加载、删除、循环切换
- `Ctrl+Z` 切换到下一个配置
- 系统托盘、开机启动、最小化/关闭到托盘
- 单实例运行；再次启动会唤醒已有窗口
- 自动保存、错误日志和旧 C# 配置迁移

## 行为说明

- 独立循环：每个启用按键分别计算并完成自己的循环次数。
- 顺序循环：按列表顺序发送全部启用按键，一次完整遍历算一轮。
- 基础随机范围为“按键浮动 + 全局浮动”，并在基础间隔两侧取值。
- “节奏变化”会在配置的随机范围内做节奏漂移、相关性处理和少量微偏移，不主动插入额外短暂停顿。
- 调度按按键开始时间计算下一次随机间隔；如果按键持续时间或排队已经耗掉间隔，会尽快继续下一次按键。
- 启动时会复制当前配置作为运行快照。运行期间编辑界面只影响下一次启动，不会重置当前调度。
- 停止发生在按键按下期间时，程序会先尝试发送对应的 KeyUp，降低按键卡住风险。

## 构建与检查

要求 Windows 10/11、Rust stable 和 MSVC C++ 构建工具。

```powershell
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --release
```

`build.bat` 还会运行隔离的 packaged startup smoke，验证快捷方式启动、
`--autostart` 隐藏、第二实例唤醒和测试资源清理，然后将产物复制到 `dist`。

输出文件：

```text
target\release\AutoKeyRust.exe
```

## 使用

1. 启动程序。
2. 点击按键行中的按键按钮，然后按下目标键；按 Esc 清除。
3. 设置间隔、浮动、循环模式和启用状态。
4. 点击“启动”，或单独按下左 Alt。
5. 如需后台窗口消息，使用窗口列表、`Ctrl+Alt+Space` 或右键拖动进行绑定。

`Ctrl+Z` 会按配置列表顺序切换到下一个配置。该快捷键当前为固定行为，不在界面中配置。

### 开机自启

- 普通权限运行时，程序使用当前用户 Startup 文件夹中的 `AutoKey-Rust.lnk`。
- 管理员权限运行或 exe 设置了 `RUNASADMIN` 时，程序使用当前用户登录触发、最高权限运行的计划任务，避免登录阶段无法显示 UAC 导致启动被跳过。
- 切换自启时会验证目标 exe、`--autostart` 参数、工作目录和 Windows 禁用状态，并清理本程序旧版 Run 注册表项。
- 从管理员计划任务切回普通快捷方式，或删除管理员计划任务时，应以管理员身份运行程序。

## 数据位置

```text
%APPDATA%\AutoKey-Rust\configs\*.json
%APPDATA%\AutoKey-Rust\app-state.json
%APPDATA%\AutoKey-Rust\logs\error.log
%APPDATA%\AutoKey-Rust\logs\app.log
```

首次运行会尝试迁移：

```text
%APPDATA%\KeyScheduler\configs\*.json
%APPDATA%\KeyScheduler\app-state.json
%APPDATA%\AutoKey\configs\*.json
%APPDATA%\AutoKey\app-state.json
```

## 输入与安全边界

本项目的按键发送只使用公开、标准的 Windows 输入接口。前台主按键输入使用 `SendInput`，后台窗口输入使用 `PostMessage`；当前源码会对主按键 `SendInput` 的 `dwExtraInfo` 和后台 `PostMessage` keydown 的部分 `lParam` 位做随机扰动。Hook 内部用于释放修饰键或回放 Alt 状态的少量辅助 `SendInput` 事件仍使用 `dwExtraInfo = 0`。

当前源码还包含部分运行时标识扰动，例如编译期字符串异或、随机线程名、启动时调试器/分析工具标志检测；这些是当前实现状态，不代表能够规避目标软件、安全软件或系统级检测。

项目不实现也不承诺绕过反作弊、安全检测、API 监控、权限边界或目标软件规则。Windows 和目标程序仍可能识别合成输入。请只在获得授权且符合软件条款的自动化场景中使用。

后台 `PostMessage` 是否有效由目标程序的消息处理方式决定；管理员权限程序通常要求本程序也以同等权限运行。

检测相关承诺与当前实现的逐项核查见 [DETECTION_AUDIT.md](DETECTION_AUDIT.md)。
