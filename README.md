# AutoKey-Rust

AutoKey-Rust 是一个 Windows 桌面按键调度工具，使用 Rust、egui 和标准 Win32 API 实现。

## 功能

- 12 个独立按键槽，可点击捕获普通键、数字键、方向键、小键盘键和 F1-F24
- 每个按键可单独启用，并配置基础间隔和随机浮动
- 独立循环与顺序循环
- 手动停止或 1、2、3、5、10、50、100 次循环
- 前台 `SendInput` 和绑定窗口 `PostMessage` 两种发送方式
- 左 Alt 单独按下切换启动/停止
- `Ctrl+Alt+Space` 绑定鼠标所在窗口
- 按住鼠标右键拖动超过 8 像素后绑定目标窗口
- 多配置保存、加载、删除、循环切换
- 每个配置独立热键，以及全局“下一个配置”热键
- 系统托盘、开机启动、最小化/关闭到托盘
- 单实例运行；再次启动会唤醒已有窗口
- 自动保存、错误日志和旧 C# 配置迁移

## 行为说明

- 独立循环：每个启用按键分别计算并完成自己的循环次数。
- 顺序循环：按列表顺序发送全部启用按键，一次完整遍历算一轮。
- 基础随机范围为“按键浮动 + 全局浮动”，并在基础间隔两侧取值。
- “节奏变化”设为“明显”时，会保留旧版的缓慢节奏漂移和偶发短暂停顿，因此少量间隔可能高于基础随机范围。
- 启动时会复制当前配置作为运行快照。运行期间编辑界面只影响下一次启动，不会重置当前调度。
- 停止发生在按键按下期间时，程序仍会先发送对应的 KeyUp，避免按键卡住。

## 构建与检查

要求 Windows 10/11、Rust stable 和 MSVC C++ 构建工具。

```powershell
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --release
```

输出文件：

```text
target\release\autokey.exe
```

## 使用

1. 启动程序。
2. 点击按键行中的按键按钮，然后按下目标键；按 Esc 清除。
3. 设置间隔、浮动、循环模式和启用状态。
4. 点击“启动”，或单独按下左 Alt。
5. 如需后台窗口消息，使用窗口列表、`Ctrl+Alt+Space` 或右键拖动进行绑定。

配置热键可直接输入，也可点击“捕获”。格式示例：

```text
Ctrl+Z
Ctrl+Shift+F5
Alt+PageDown
Win+VK186
```

## 数据位置

```text
%APPDATA%\AutoKey-Rust\configs\*.json
%APPDATA%\AutoKey-Rust\app-state.json
%APPDATA%\AutoKey-Rust\logs\error.log
```

首次运行会尝试迁移：

```text
%APPDATA%\AutoKey\configs\*.json
%APPDATA%\AutoKey\app-state.json
```

## 输入与安全边界

本项目只使用公开、标准的 Windows 输入接口。前台输入使用 `SendInput`，后台窗口输入使用 `PostMessage`，`dwExtraInfo` 为 `0`。

项目不实现也不承诺绕过反作弊、安全检测、API 监控、权限边界或目标软件规则。Windows 和目标程序仍可能识别合成输入。请只在获得授权且符合软件条款的自动化场景中使用。

后台 `PostMessage` 是否有效由目标程序的消息处理方式决定；管理员权限程序通常要求本程序也以同等权限运行。
