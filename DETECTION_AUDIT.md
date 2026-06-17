# 检测相关审计

本文对照 `info.txt` 中的检测相关说法、旧 C# 项目 `E:\Project\Common\AutoKey`、以及当前 Rust 实现。结论只描述当前实现状态，不承诺绕过反作弊、安全检测、API 监控、权限边界或目标软件规则。

## 结论摘要

当前 Rust 版保留了标准 Windows 输入路径和透明运行特征：前台输入使用 `SendInput`，后台窗口输入使用 `PostMessage`，`dwExtraInfo` 固定为 `0`。它不实现直接 syscall、输入来源伪装、字符串隐藏、进程隐藏或内存混淆。

与旧 C# 版相比，Rust 版已经消除了 `.NET/CLR/JIT/GC` 运行时特征，并移除了旧 C# 的固定 `0x41554B59` 输入标记。当前代码还包含节奏变化、马尔可夫延迟关联和 QPC 高精度计时，但这些只作为调度稳定性与时序变化能力存在，不作为安全检测规避保证。

## 逐项核查

| `info.txt` 说法 | 当前状态 | 证据 | 结论 |
| --- | --- | --- | --- |
| `dwExtraInfo` 随机化为 `0` 或小随机值 | 未实现 | `src/input.rs` 将 `dwExtraInfo` 设置为 `0` | 旧 C# 固定 `0x41554B59` 标记已移除，但没有随机化 |
| 直接 syscall 调用 `NtUserSendInput` | 未实现 | 当前无 `src/syscall.rs`，`src/input.rs` 直接调用标准 `SendInput` | 上轮已移除此类规避 API 监控的实现 |
| 马尔可夫链模拟延迟关联 | 已实现 | `src/humanizer.rs` 有 `MarkovChain`、8 状态转移矩阵和相关测试 | 当前实现存在，但不应宣传为检测绕过能力 |
| QPC 高精度定时器 | 已实现 | `src/engine.rs` 使用 `QueryPerformanceCounter`、`QueryPerformanceFrequency`、`timeBeginPeriod(1)` | 当前实现存在，用于更稳定的等待与更快停止响应 |
| 字符串混淆、随机线程名、移除可识别字符串 | 未实现 | 当前线程名如 `autokey-engine`，UI/文档使用明确产品名 | 上轮已移除混淆模块，选择透明可诊断实现 |
| 内存混淆/内存保护 | 未实现 | 当前无 `VirtualLock`、运行时解密字符串、secure string 或 zeroize 实现 | 不实现此类隐藏/混淆能力 |
| `LLKHF_INJECTED` 处理 | 已实现一部分 | `src/hook.rs` 只处理非注入键盘事件 | 用于避免处理合成输入触发的 hook 事件，不用于隐藏自身输入 |
| `PostMessage` lParam 随机化 | 未实现 | `src/input.rs` 按 Win32 常规字段构造 lParam | 上轮已移除随机 repeat/context 位，保留标准消息结构 |
| 进程特征从 C# 运行时变为原生程序 | 已实现 | Rust release 构建为原生 exe，无 .NET 运行时依赖 | 这是可确认的架构变化 |
| 配置目录改为 `KeyScheduler` | 不作为当前目录 | 当前主目录为 `%APPDATA%\AutoKey-Rust`，兼容迁移 `%APPDATA%\KeyScheduler` | README 与当前实现一致；旧目录只用于迁移 |

## 上轮改动影响

上轮改动确实移除了这些更偏规避检测的实现或承诺：

- 删除直接 syscall 输入路径，恢复标准 `SendInput`。
- 删除随机 `dwExtraInfo`，固定为 `0`。
- 删除 `PostMessage` lParam 随机 repeat/context 位。
- 删除字符串混淆和随机线程名模块。
- 将配置目录从 `KeyScheduler` 改回文档一致的 `%APPDATA%\AutoKey-Rust`，并保留旧目录迁移。

这些变化会降低“隐藏调用路径、隐藏字符串、模拟输入来源随机特征”一类能力，但提高了可维护性、可诊断性、文档真实性和安全边界清晰度。项目当前明确只支持授权场景下的标准自动化，不支持检测绕过。

## 当前可验证边界

- 标准输入：`SendInput` / `PostMessage`。
- 输入标记：`dwExtraInfo = 0`，不使用固定旧标记，也不随机伪装。
- 时序：可配置随机范围、节奏变化、马尔可夫延迟关联、QPC 等待。
- Hook：只处理物理键盘事件，避免合成输入反馈到快捷键状态机。
- 文档：README 的“输入与安全边界”是当前项目的准确信息源。
