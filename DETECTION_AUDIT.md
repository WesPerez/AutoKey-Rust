# 检测相关审计

本文对照 `info.txt` 中的检测相关说法、旧 C# 项目 `E:\Project\Common\AutoKey`、以及当前 Rust 实现。结论只描述当前实现状态，不承诺绕过反作弊、安全检测、API 监控、权限边界或目标软件规则。

## 结论摘要

当前 Rust 版保留了标准 Windows 输入路径：前台主按键输入使用 `SendInput`，后台窗口输入使用 `PostMessage`。当前源码会对主按键 `SendInput` 的 `dwExtraInfo` 和后台 `PostMessage` keydown 的部分 `lParam` 位做随机扰动；Hook 内部用于释放修饰键或回放 Alt 状态的少量辅助 `SendInput` 事件仍使用 `dwExtraInfo = 0`。

当前源码还包含编译期字符串异或、随机线程名、启动时调试器/分析工具标志检测和少量内存清零 helper。它不实现直接 syscall、驱动、内核组件、进程隐藏或权限绕过。与旧 C# 版相比，Rust 版已经消除了 `.NET/CLR/JIT/GC` 运行时特征，并移除了旧 C# 的固定 `0x41554B59` 输入标记。节奏变化、马尔可夫延迟关联和 QPC 高精度计时作为调度稳定性与时序变化能力存在，不作为安全检测规避保证。

## 逐项核查

| `info.txt` 说法 | 当前状态 | 证据 | 结论 |
| --- | --- | --- | --- |
| `dwExtraInfo` 随机化为 `0` 或小随机值 | 已实现一部分 | `src/input.rs` 的主按键 `SendInput` 使用 `stealth::random_extra_info()`；`src/hook.rs` 的辅助合成事件仍为 `0` | 旧 C# 固定 `0x41554B59` 标记已移除；当前主按键路径会随机化 |
| 直接 syscall 调用 `NtUserSendInput` | 未实现 | 当前无 `src/syscall.rs`，`src/input.rs` 直接调用标准 `SendInput` | 上轮已移除此类规避 API 监控的实现 |
| 马尔可夫链模拟延迟关联 | 已实现 | `src/humanizer.rs` 有 `MarkovChain`、8 状态转移矩阵和相关测试 | 当前实现存在，但不应宣传为检测绕过能力 |
| QPC 高精度定时器 | 已实现 | `src/engine.rs` 使用 `QueryPerformanceCounter`、`QueryPerformanceFrequency`、`timeBeginPeriod(1)` | 当前实现存在，用于更稳定的等待与更快停止响应 |
| 字符串混淆、随机线程名、移除可识别字符串 | 已实现一部分 | `src/stealth.rs` 提供 `obfstr!` 和 `random_thread_name()`；engine、hook、GUI 辅助线程等使用随机线程名 | 仅覆盖部分运行时字符串和线程名，文档/资源/产品名仍可识别 |
| 内存混淆/内存保护 | 已实现极少部分 | `src/stealth.rs` 有 `secure_zero()` helper，但当前没有 `VirtualLock` 或系统性 secure string 机制 | 只有内存清零 helper，不是完整内存保护方案 |
| `LLKHF_INJECTED` 处理 | 已实现一部分 | `src/hook.rs` 只处理非注入键盘事件 | 用于避免处理合成输入触发的 hook 事件，不用于隐藏自身输入 |
| `PostMessage` lParam 随机化 | 已实现 | `src/input.rs` 调用 `stealth::randomize_lparam()`；keydown 随机 reserved bits，且偶尔 repeat count 为 2；keyup 保持标准 lParam | 当前实现存在；仍使用标准 `PostMessage` 路径，不保证目标程序接受或无法识别 |
| 进程特征从 C# 运行时变为原生程序 | 已实现 | Rust release 构建为原生 exe，无 .NET 运行时依赖 | 这是可确认的架构变化 |
| 配置目录改为 `KeyScheduler` | 不作为当前目录 | 当前主目录为 `%APPDATA%\AutoKey-Rust`，兼容迁移 `%APPDATA%\KeyScheduler` | README 与当前实现一致；旧目录只用于迁移 |

## 当前实现影响

当前源码保留和不保留的边界如下：

- 不存在直接 syscall 输入路径，主按键仍调用标准 `SendInput`。
- 保留主按键 `dwExtraInfo` 随机化、`PostMessage` lParam 随机化、字符串异或和随机线程名。
- 将配置目录从 `KeyScheduler` 改回文档一致的 `%APPDATA%\AutoKey-Rust`，并保留旧目录迁移。

项目当前仍明确只支持授权场景下的标准自动化，不承诺检测绕过。文档里的安全边界应以“当前源码实际状态 + 不作规避保证”为准。

## 当前可验证边界

- 标准输入：`SendInput` / `PostMessage`。
- 输入标记：主按键 `SendInput` 使用随机 `dwExtraInfo`，Hook 辅助合成事件使用 `dwExtraInfo = 0`。
- 后台消息：`PostMessage` keydown lParam 当前包含 reserved bits 随机化和少量 repeat count 扰动；keyup lParam 保持标准。
- 时序：可配置随机范围、节奏变化、马尔可夫延迟关联、QPC 等待。
- Hook：只处理物理键盘事件，避免合成输入反馈到快捷键状态机。
- 运行时标识：部分字符串异或、随机线程名、启动时调试器/分析工具标志检测。
- 不存在：直接 syscall、驱动/内核组件、进程隐藏、权限绕过、目标软件规则绕过保证。
