# 0002 C# WinUI 3 界面外壳 + Rust 核心 + Python 豆包桥接

- 日期：2026-11-06
- 状态：采用

## 背景

需求文档要求核心用 Rust、界面用 WinUI 3，并建议先验证「C++/WinRT 承载 WinUI 3、Rust 提供核心逻辑」。默认识别服务是现有的 Python 模块 `doubao_asr`。界面需要透明、不抢焦点的浮层，托盘，全局快捷键和单实例。

## 决定

- **界面**：C# WinUI 3（Windows App SDK 2.5，.NET 10，非打包、自包含）。界面全部用 C# 代码构建，`App.xaml` 只放资源。
- **核心**：Rust 编译成 `livelisten_native.dll`，导出 5 个 C 函数，命令和事件都用 JSON 字符串传递。音频、钩子、输入、焦点判断、服务进程和设置都在 Rust 中。
- **识别服务**：独立进程，按 [识别服务协议](../PROVIDER_PROTOCOL.md) 通信。豆包通过 `providers/doubao_live.py` 调用 `doubao_asr`，本轮不移植到 Rust，也不修改 `doubao_asr`。

## 考虑过的方案

| 方案 | 结论 |
| --- | --- |
| C++/WinRT 承载 WinUI 3 | 可行，但 XAML 编译、事件和生命周期样板代码多，开发和调试慢；C# 是微软对 WinUI 3 的主要支持路径 |
| Rust 直接调用 WinUI 3（windows-rs） | WinUI 3 的 XAML 类型没有成熟的 Rust 绑定，风险高 |
| Tauri 2 网页界面 | 不是原生 Fluent 界面；透明且不激活的浮层不好做；内存开销更大 |
| 豆包识别移植到 Rust | 需求列为后续任务；先用 Python 对照实现保证结果一致 |
| 一个服务进程同时处理听写和字幕 | 会话会互相排队；改为每个音频通道一个进程 |

## 影响

- 好处：界面只是外壳，业务逻辑集中在 Rust 核心，可以用 CLI 和单元测试覆盖；识别服务可替换。
- 代价：运行时同时需要 .NET 运行库（已自包含）和 Python；进程数量增加（界面 + 每个通道一个 Python 进程）。
- 教训：原生 DLL 起初叫 `livelisten.dll`，在 Windows 不区分大小写的文件系统上与托管程序集 `LiveListen.dll` 同名，`DllImport` 加载到托管程序集，启动即因找不到 `ll_start` 崩溃。改名为 `livelisten_native.dll`，并在加载失败时显示「核心组件缺失」而不是直接崩溃。
- 后续：豆包移植到 Rust 后可去掉 Python 依赖；发布前需要签名、安装包和更新方案。
