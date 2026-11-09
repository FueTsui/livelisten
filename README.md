<div align="center">

<img src="docs/images/icon.png" width="96" alt="实时聆听">

# 实时聆听

**Windows 11 语音输入、实时字幕与随手记**

说话即写入当前输入框；电脑里播放的声音实时变成字幕；一句话记成桌面卡片。

![Version](https://img.shields.io/badge/version-0.1.3-blue)
[![GitHub Release](https://img.shields.io/github/v/release/FueTsui/livelisten)](https://github.com/FueTsui/livelisten/releases/latest)
![Platform](https://img.shields.io/badge/platform-Windows%2011%20x64-0078D4)
![Rust](https://img.shields.io/badge/rust-1.85%2B-orange)
![.NET](https://img.shields.io/badge/.NET-10-512BD4)
![License](https://img.shields.io/badge/license-proprietary-red)

[English](README.en.md) · [下载](https://github.com/FueTsui/livelisten/releases/latest) · [安装与卸载](docs/INSTALLATION.md) · [使用说明](docs/USAGE.md) · [官方识别服务](docs/OFFICIAL_SPEECH_PROVIDERS.md) · [更新日志](CHANGELOG.md)

</div>

**适用于 Windows 11 x64。** 安装包、便携包和 SHA-256 校验值可从 GitHub Releases 下载。

## 特性

- **听写**：按住右 <kbd>Ctrl</kbd> 说话、松开结束，文字写入当前输入框；也可切换为持续听写。可选稳定写入或实时修正，结束时可附带回车等按键。
- **实时字幕**：音源可选电脑声音、麦克风或两者混合；字号、宽度和外观可调，修改即时生效。
- **随手记与待办**：一句话记成桌面卡片，卡片可编辑、完成、归档、改日期，并可单独设置外观。
- **语音浮条**：星云与星河两种透明语音动效，可拖动、从中心缩放；靠近屏幕底部时文字自动移到上方。
- **提前连接**：可选关闭、均衡、积极三档，在后台准备好识别连接，让第一个字更快出现。
- **多种识别服务**：内置豆包兼容通道（免配置），以及豆包、OpenAI、千问、腾讯官方接口（使用自己的凭据）。
- **写入保护**：密码框禁止听写；输入目标切换或无法写入时暂停，未写入的文字转到剪贴板，并明确提示成功或失败。
- **托盘常驻**：关闭浮条后继续在托盘运行，选择「退出」才结束；可设置开机启动。
- **安装包**：「立即安装」一键完成，升级保留设置、卡片与密钥，失败时自动恢复旧版本。

## 语音动效预览

| 星云 | 星河 |
| :---: | :---: |
| ![星云光环与粒子随音量变化](docs/images/voice-nebula.gif) | ![星河的多层波形随音量起伏](docs/images/voice-galaxy.gif) |

动图由应用内的 WinUI 控件渲染，使用模拟音量展示听写时的变化。

## 安装

从 [GitHub Releases](https://github.com/FueTsui/livelisten/releases/latest) 下载安装包 `LiveListen-Setup-<版本号>-x64.exe`，或选择便携包 `LiveListen-<版本号>-portable-x64.zip`。下载同一版本的 `SHA256SUMS.txt` 核对文件。

1. **先核对校验值**：

   ```powershell
   Get-FileHash .\LiveListen-Setup-0.1.3-x64.exe -Algorithm SHA256
   ```

   结果应与 `SHA256SUMS.txt` 中对应行一致；不一致时不要运行。
2. 运行安装包，点击「立即安装」。「自定义选项」中可修改安装位置、快捷方式、开机启动和安装后运行。
3. 程序未签名，Windows 可能显示 SmartScreen 提示。只在确认来源和校验值可信后，点击「更多信息 → 仍要运行」；不要关闭任何安全功能。

- 系统要求：Windows 11 x64；需要可用的音频设备和麦克风权限，在线识别需要网络。
- 默认安装到 `%LOCALAPPDATA%\Programs\LiveListen`，并创建开始菜单快捷方式。
- 升级：运行新版安装包即可。正在运行的程序会先被请求正常退出；保留设置、卡片、密钥以及快捷方式和开机启动选项。替换失败时恢复全部原文件，旧版本仍可使用。
- 卸载：在 Windows「已安装的应用」中找到「LiveListen」并卸载，或运行安装包并点击窗口右下角的「卸载 v版本号」。卸载保留设置、卡片、随手记文档和密钥。
- 便携包：完整解压到新的普通文件夹后运行 `LiveListen.exe`，不要只复制 EXE 或在压缩包内运行。

包内已带 .NET、Windows App SDK、嵌入式 Python 及依赖，无需另装运行时。静默安装参数、退出码和日志位置见 [安装与卸载](docs/INSTALLATION.md)。

## 快速使用

启动后显示语音浮条并常驻托盘。先在设置中选择麦克风和识别服务，然后在一个空白文本框里试用：

| 快捷键 | 功能 |
| --- | --- |
| 按住右 <kbd>Ctrl</kbd> | 听写：说话写入当前输入框，松开结束 |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Space</kbd> | 开始或结束持续听写 |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>C</kbd> | 开关实时字幕 |
| 按住 <kbd>Ctrl</kbd>+<kbd>→</kbd> | 录随手记，松开右方向键后结束并保存 |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>N</kbd> | 启动托盘同款随手记录音，再按或按普通键结束 |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>L</kbd> | 在桌面新建空白随手记并打开编辑器 |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>K</kbd> | 显示或隐藏卡片 |

- 快捷键可在设置中直接点击键值输入框录入，松开按键后保存；按 Esc 或移开焦点取消，输入框右侧的清除按钮可禁用该项。
- 单击托盘图标显示或隐藏浮条。右键菜单的「显示听写」与听写设置中的「浮条显示实时文字」同步。
- 服务的「检查」只检查本地配置和桥接进程能否启动，不代表云端鉴权或识别一定成功。

### 识别服务

| 服务 | 凭据 | 说明 |
| --- | --- | --- |
| 内置豆包兼容通道 | 不需要 | 基于第三方 MIT 模块，依赖网页服务行为，服务变化时可能不可用 |
| 豆包官方 | API Key | 默认模型 `bigmodel` |
| OpenAI | API Key | 默认模型 `gpt-live-transcribe` |
| 千问 | API Key | 默认模型 `qwen3-asr-flash-realtime`，可选北京或新加坡地域 |
| 腾讯 | SecretId 与 SecretKey，AppId 填在设置中 | 默认引擎 `16k_zh` |

官方接口的地址与字段见 [官方识别服务](docs/OFFICIAL_SPEECH_PROVIDERS.md)。

服务账号、额度和费用由服务商提供。本版本没有字幕翻译、会议摘要、声纹识别或模型下载。

### 提前连接

| 档位 | 行为 |
| --- | --- |
| 关闭（默认） | 开始听写时才连接 |
| 均衡 | 每次听写后的 2 分钟内保持一个就绪连接，按住听写键时也会提前连接 |
| 积极 | 启动后和每次听写后的 10 分钟内保持就绪，首字最快，连接次数最多 |

准备连接时不采集也不发送音频，但服务端仍可能进行鉴权。离开电脑（含锁屏）超过均衡档 1 分钟、积极档 3 分钟后暂停，回来有输入时恢复。

更多操作见 [使用说明](docs/USAGE.md)。

## 数据去向

| 数据 | 去向与保留 |
| --- | --- |
| 麦克风或电脑声音 | 只发给当前选择的识别服务；不保存录音，断线重连时内存中最多暂存 90 秒 |
| 听写文字 | 写入你选择的输入框；未写入的部分进入 Windows 剪贴板，可能受系统剪贴板历史和同步设置影响 |
| 实时字幕 | 只在内存中保留最近内容，不保存 |
| 随手记 / 待办 | `%APPDATA%\LiveListen\cards.json` |
| 设置 | `%APPDATA%\LiveListen\settings.json`，不含服务密钥 |
| 官方服务凭据 | Windows 凭据管理器；只在启动对应识别进程时传入，不写入命令行或日志 |
| 诊断日志 | `%LOCALAPPDATA%\LiveListen\Logs`；不上传，只记录事件、错误码和长度，不记录识别文字或按键 |
| 安装日志 | `%TEMP%\LiveListen-Setup-<时间>.log` / `LiveListen-Uninstall-<时间>.log` |
| 检查更新 | 每天检查一次并提醒新版。在设置「通用」手动检查后，安装版下载并验证安装包，再自动升级。便携版下载 ZIP，解压后使用 |

更多见 [SECURITY.md](SECURITY.md)。

## 接口

界面通过 JSON C ABI 调用 Rust 核心；同一核心还提供命令行程序。程序不提供 HTTP 服务。

### 能力一览

| 能力 | SDK（`ll_call` 命令） | CLI 命令 | HTTP 端点 | 副作用等级 |
| --- | --- | --- | --- | --- |
| 设备与服务列表 | `devices`、`providers` | `devices`、`providers` | — | 只读 |
| 本地服务检查 | `provider.check` | `check` | — | 启动本地桥接进程 |
| 一次性识别 | — | `listen` | — | 音频外发 |
| 设置 | `settings.get`、`settings.set` | `config`（只读） | — | 读取 / 本地写入 |
| 听写 | `dictation.start/toggle/stop/cancel` | — | — | 音频外发、写入目标输入框 |
| 字幕 | `captions.toggle/set/lines` | — | — | 音频外发 |
| 语音随手记 | `note.toggle` | — | — | 音频外发、本地写入卡片 |
| 卡片 | `cards.list/add/update/styles.clear/show/delete` | — | — | 本地写入；删除不可恢复 |
| 凭据 | `secret.set/delete/status` | — | — | 写入 Windows 凭据管理器 |
| 诊断 | `diagnostics.export` | — | — | 本地文件写入 |

### SDK

`livelisten_native.dll`（x64，随程序发布）导出 C ABI，所有请求和回复都是 UTF-8 JSON：

```c
typedef void (*ll_event)(const char *json, void *user);
LLHandle  *ll_start(const char *config_dir /* UTF-8 或 NULL */, ll_event callback, void *user);
char      *ll_call(LLHandle *handle, const char *request_json);   // 返回值用 ll_free 释放
void       ll_free(char *text);
void       ll_destroy(LLHandle *handle);
const char *ll_version(void);
```

- 请求形如 `{"cmd":"providers"}`；成功回复带 `"ok":true`，失败为 `{"ok":false,"error":"<错误码>"}`。
- `ll_start` 失败时返回 NULL。事件在引擎线程上回调，回调应尽快返回并复制字符串。
- `cards.add` 可传入 `"editOnDesktop":true`，显示桌面卡片并在 `cards.changed` 事件中返回待编辑的 `editCard`。
- 文本协议见 [识别桥接协议](docs/PROVIDER_PROTOCOL.md)。

### CLI

`live-listen.exe` 位于安装目录，每次执行一条命令后退出：

```powershell
live-listen devices --json
live-listen providers
live-listen check --provider doubao
live-listen listen --wav .\sample.wav --json
live-listen config
```

| 项目 | 说明 |
| --- | --- |
| 通用选项 | `--config DIR` 指定设置目录（默认 `%APPDATA%\LiveListen`）；`--json` 每行输出一个 ASCII 转义的 JSON 对象；`--version` |
| `listen` 选项 | `--source mic\|system\|mixed`、`--device NAME`、`--wav PATH`（16 kHz 单声道 PCM16）、`--seconds N`（默认 10）、`--provider ID` |
| 退出码 | 0 成功；1 运行失败（设备、服务或识别错误）；2 参数错误 |
| 超时 | `check` 等待服务启动最多 20 秒；`listen` 在录音或 WAV 结束后最多再等 8 秒最终结果 |

`listen` 会打开音频设备并把音频发给识别服务，不是离线检查。

### 安装程序

| 参数 | 作用 |
| --- | --- |
| `/SILENT` | 只显示进度窗口，不提问 |
| `/VERYSILENT` | 不显示窗口 |
| `/SUPPRESSMSGBOXES` | 静默模式下不弹任何消息框，以退出码为准 |
| `/NORESTART` | 接受但无作用，从不需要重启 |
| `/DIR=<文件夹>` | 安装位置，仅用于全新安装 |
| `/LOG[=<文件>]` | 日志位置；不指定时也会写入临时文件夹 |
| `/TASKS=<列表>` | `startmenuicon`、`desktopicon`、`startup`，前缀 `!` 表示关闭 |
| `/UPDATE` | 只升级已有安装；程序在运行时等它退出，之后重新启动 |
| `/NOLAUNCH` | 完成后不启动程序；静默安装本来就不启动 |
| `/VERIFY` | 只校验安装包后退出 |
| `/UNINSTALL` | 使用当前安装包卸载已有安装并保留用户数据 |

退出码：0 成功；1 安装包损坏或参数无效；2 用户取消；3 失败并已回滚；7 无法关闭正在运行的程序，未替换任何文件。卸载程序支持 `/SILENT`、`/VERYSILENT`、`/SUPPRESSMSGBOXES` 和 `/LOG`，并保留设置、随手记卡片、Markdown 文档和已保存的密钥。脚本中请用 `Start-Process -Wait -PassThru` 读取退出码。

### 启动参数

| 参数 | 作用 |
| --- | --- |
| 无 | 显示语音浮条并常驻托盘 |
| `--background` | 只在托盘运行 |
| `--settings` | 打开设置 |
| `--voice-style=orb` / `--voice-style=ribbon` | 切换为星云 / 星河样式并保存 |
| `--quit` | 让正在运行的实例保存后退出 |

程序单实例运行，再次启动时把参数交给已运行的实例。

### HTTP API

无，程序不监听网络端口。

### 配置与凭据

| 名称 | 作用 |
| --- | --- |
| `%APPDATA%\LiveListen\settings.json` | 设置，结构版本 6，`dictation.preconnectLevel` 为 `off`、`balanced` 或 `aggressive` |
| Windows 凭据管理器 | 官方识别服务的密钥，在设置中填写；不进入配置文件、命令行或日志 |
| `LIVE_LISTEN_CONFIG_DIR` | 测试用：设置、卡片和日志改放到该目录，作为独立实例运行，不写开机启动项；凭据仍与正式实例共用 |
| `LIVE_LISTEN_TEST_AUDIO` / `LIVE_LISTEN_TEST_CAPTIONS_AUDIO` | 测试用：用 WAV 文件代替听写 / 字幕音频；音频仍会发给真实识别服务 |
| `LIVE_LISTEN_DEBUG` | 设为任意值时日志包含调试行 |
| `SETUP_TEST_ROOT` | 仅调试版安装程序：安装位置、开始菜单、AppData 和注册表全部改到该文件夹；发布版忽略 |

开发和构建不需要任何凭据文件。

### 兼容性

- 平台：见下方平台级别。
- 设置结构版本 3；从版本 2 升级时，原「提前连接」开关为开的会转为「积极」。旧版程序读取新设置时提前连接视为关闭。
- 识别桥接协议 v1，见 [PROVIDER_PROTOCOL.md](docs/PROVIDER_PROTOCOL.md)。
- C ABI 与 CLI 在 0.x 期间可能调整，变更记录在 [CHANGELOG](CHANGELOG.md)。

## 平台级别

| 平台 | 级别 | 状态 |
| --- | --- | --- |
| Windows 11 x64 | Tier 1 | 构建与原生回归环境；全新系统安装仍需独立验收 |
| Windows 10 x64 | 未验证 | 编译最低 API 目标为 19041，未承诺支持 |
| Windows ARM64 | 不支持 | 没有 ARM64 包 |
| macOS、Linux | 不支持 | 依赖 WinUI 3、Win32 快捷键和输入 |

## 从源码构建

**准备环境**

- Rust MSVC 工具链（1.85 或更高）与 Visual Studio C++ 生成工具、Windows SDK
- .NET 10 SDK
- Python 3.11 或更高
- PowerShell

**开发与检查**

```powershell
.\scripts\build.ps1
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked -- --skip secrets::tests::store_read_and_delete
python -B -m unittest discover -s providers\tests -v
dotnet run --project app\LiveListen.Checks -c Release
```

**打包**

```powershell
# 生成便携包、安装包、SHA256SUMS.txt、发布清单与来源记录
.\scripts\package-portable.ps1 -OutputDirectory <新的输出目录>
```

<details>
<summary>构建说明</summary>

- 被跳过的 `secrets::tests::store_read_and_delete` 会读写 Windows 凭据管理器，不计为通过。
- 打包脚本拒绝复用已有输出目录；校验固定版本 CPython 与 wheel 的 SHA-256，加入许可文本与逐文件清单。
- 安装包由 `setup` crate 生成：先打包程序文件，再追加到安装程序后面，整体 SHA-256 在安装前校验。发布版不读取 `SETUP_TEST_ROOT`。
- 发布构建静态链接 C 运行库，不依赖 VC++ 运行库。
- 不要创建或提交 `.env`、私钥、会话或用户数据。发布流程见 [RELEASE.md](docs/RELEASE.md)。

</details>

<details>
<summary>项目结构</summary>

```text
crates/live-listen-core/   业务逻辑：音频、快捷键、输入、听写、字幕、识别进程、设置、凭据、卡片
crates/live-listen-ffi/    C ABI：livelisten_native.dll
crates/live-listen-cli/    命令行程序 live-listen.exe
app/LiveListen/            WinUI 3 界面（只做显示与交互）
app/LiveListen.Checks/     界面层回归检查
providers/                 识别桥接（Python，识别桥接协议 v1）
setup/                     安装程序与卸载程序（egui）
scripts/                   构建、打包与发布检查
docs/                      使用、安装、架构、威胁模型、决策记录与验证记录
```

</details>

## 文档

| 文档 | 内容 |
| --- | --- |
| [使用说明](docs/USAGE.md) | 各项功能的详细行为 |
| [安装与卸载](docs/INSTALLATION.md) | 安装包、便携包、升级、静默参数与卸载 |
| [官方识别服务](docs/OFFICIAL_SPEECH_PROVIDERS.md) | 豆包、OpenAI、千问、腾讯接口配置 |
| [架构](docs/ARCHITECTURE.md) | 整体结构与模块划分 |
| [识别桥接协议](docs/PROVIDER_PROTOCOL.md) | 核心与识别进程之间的消息格式 |
| [威胁模型](docs/THREAT_MODEL.md) | 数据、权限与残余风险 |
| [决策记录](docs/adr/) | 关键设计取舍 |
| [发布记录](https://github.com/FueTsui/livelisten/releases) | 版本变更、下载与验证范围 |

## 已知限制

- 程序未签名，首次运行可能出现 SmartScreen 提示。
- 内置豆包兼容通道依赖网页服务行为，可能随服务变化失效。
- 输入法、远程桌面和部分外部应用的写入组合未全部验收；Qt 应用使用消息方式写入。
- 全新 Windows、Windows 10 和 ARM64 尚未验证。

## 规范例外

采用个人开发与文件管理规范 1.0.1，例外如下。

| 规则编号 | 原因 | 范围 | 替代措施 | 到期或复查日期 |
| --- | --- | --- | --- | --- |
| SUP-01～SUP-05、ENG-11、REL-20 | 暂无 CI，构建在本机完成 | 全部构建与发布包 | 本地检查日志、固定依赖、逐文件清单与 SHA-256 | 首次公开发布前 |
| REL-10、REL-21 | 暂无可用的发布签名密钥 | 安装包、便携包与发布清单 | 明确标注未签名，提供 SHA-256，更新由用户手动执行 | 恢复签名密钥访问后 |
| 8.4（安装需 UAC） | 程序与数据都属于当前用户，安装到用户目录无需提升权限 | 安装与卸载 | 只写当前用户的程序目录、开始菜单和注册表项；按完整路径识别进程 | 下一版本发布前 |
| REL-08 | 安装程序位于本仓库 `setup/`，尚未拆成共享 crate | 安装程序 | 与 SightOCR 安装程序采用同一界面与参数约定；产品信息全部来自安装清单 | 下一版本发布前 |
| GEN-05 | Rust 使用 stable 通道 | 构建工具链 | `Cargo.lock` 固定依赖，发布记录实际 rustc、dotnet、Python 版本 | 下一版本发布前 |

## 许可

Copyright © 2026 FueTsui. All Rights Reserved.

实时聆听为专有软件，源码公开，详见 [LICENSE](LICENSE)。第三方代码及运行库保留各自的许可与版权，见 [第三方组件](docs/THIRD_PARTY_NOTICES.md)；仓库访问权限不代表再分发许可。
