# 安装、升级与卸载

适用于 LiveListen 0.1.0。安装包和便携包见 [GitHub Releases](https://github.com/FueTsui/livelisten/releases/latest)。

## 系统要求

- Windows 11 x64；Windows 10、ARM64、虚拟机和全新系统尚未全面验收。
- 可用的音频设备和 Windows 麦克风权限；在线识别需要网络和服务可用。
- 官方识别接口需要你自己的服务账号、授权和额度。
- 安装包和便携包都已带 .NET、Windows App SDK、嵌入式 Python 及依赖，不需要另装运行时。
- 安装窗口使用 OpenGL；在没有可用显卡驱动的远程桌面或虚拟机中无法显示窗口时，安装程序改用系统对话框，按默认选项安装。

## 核对校验值

安装包 `LiveListen-Setup-0.1.0-x64.exe`、便携包 `LiveListen-0.1.0-portable-x64.zip` 和 `SHA256SUMS.txt` 位于同一输出目录。运行前先核对：

```powershell
Get-FileHash .\LiveListen-Setup-0.1.0-x64.exe -Algorithm SHA256
```

结果应与 `SHA256SUMS.txt` 中对应行一致，不一致时不要运行。校验值只证明文件没有变化，不能代替发布者签名或可信来源。

本版未签名。Windows 可能显示 SmartScreen 提示：先确认来源和校验值，再点击「更多信息 → 仍要运行」。不要关闭任何安全功能。

## 安装

运行安装包，点击「立即安装」即可，默认：

- 安装到 `%LOCALAPPDATA%\Programs\LiveListen`；
- 创建开始菜单快捷方式；
- 完成后可勾选「立即运行」。

点击「自定义选项」可修改安装位置，选择开始菜单快捷方式、桌面快捷方式、开机自动启动和安装完成后运行。按 <kbd>Enter</kbd> 等同主按钮，<kbd>Esc</kbd> 收起选项或取消。

安装包启动时先校验整个安装包；损坏或不完整的安装包不会写入任何文件。新的安装位置必须是完整路径，并且是空文件夹或尚不存在的文件夹；不能直接用系统或用户文件夹本身，路径过长时也会提示改选。

## 升级与修复

运行新版本安装包即可升级。窗口会显示「将从 vX 升级」，安装位置沿用当前安装。

- 程序正在运行时，窗口先提示，按钮变为「关闭并安装」。安装程序按完整路径找到这一安装中的进程，先请它正常退出（保存设置和卡片），15 秒后仍未退出才结束它。其他文件夹中同名的程序不受影响。
- 新文件全部解压并校验后才开始替换。任一文件替换失败时恢复全部原文件，旧版本仍可启动。
- 设置、卡片、密钥、快捷方式和开机启动选项保持不变；你自己放进安装目录的文件不会被删除。
- 运行同版本安装包为「重新安装」，用于修复损坏的程序文件。
- 运行较旧版本的安装包会先警告，确认后才降级。

## 卸载

以下任一方式：

- Windows「设置 → 应用 → 已安装的应用」中卸载「LiveListen」；
- 运行安装包，点击窗口右下角的「卸载 v版本号」；
- 运行安装目录中的 `Uninstall.exe`。

默认保留设置、卡片和已保存的密钥，重新安装后可继续使用。勾选「同时删除设置、随手记卡片和已保存的密钥」会删除 `%APPDATA%\LiveListen`、`%LOCALAPPDATA%\LiveListen` 以及凭据管理器中本程序的凭据，无法恢复。

卸载程序只删除安装时记录的文件和指向本安装的开机启动项。它退出后，临时文件夹中的副本会删除 `Uninstall.exe` 和空的安装目录，然后删除自身。

## 静默安装与脚本

| 参数 | 作用 |
| --- | --- |
| `/SILENT` | 只显示进度窗口，不提问；失败时显示原因 |
| `/VERYSILENT` | 不显示窗口 |
| `/SUPPRESSMSGBOXES` | 静默模式下不弹任何消息框，以退出码为准 |
| `/NORESTART` | 接受但无作用 |
| `/DIR=<文件夹>` | 安装位置，仅用于全新安装；与已有安装位置不同时失败 |
| `/LOG[=<文件>]` | 日志位置 |
| `/TASKS=<列表>` | `startmenuicon`、`desktopicon`、`startup`，逗号分隔，前缀 `!` 表示关闭 |
| `/UPDATE` | 只升级已有安装；没有安装或版本更低时失败 |
| `/NOLAUNCH` | 完成后不启动程序 |
| `/VERIFY` | 只校验安装包后退出 |

| 退出码 | 含义 |
| --- | --- |
| 0 | 成功 |
| 1 | 安装包损坏、参数无效，或条件不满足（如 `/UPDATE` 时没有安装） |
| 2 | 用户取消 |
| 3 | 失败并已回滚，原有版本仍可使用 |
| 7 | 无法关闭正在运行的程序，没有替换任何文件 |

- 静默安装不会自动启动程序。`/UPDATE` 只在升级前程序正在运行时重新启动它；`/NOLAUNCH` 禁止重新启动。
- 静默模式需要关闭正在运行的程序时，会先弹窗确认；加 `/SUPPRESSMSGBOXES` 时直接关闭。`/UPDATE` 最多等程序自行退出 120 秒，从不强制结束，超时返回 7。
- 卸载程序支持 `/SILENT`、`/VERYSILENT`、`/SUPPRESSMSGBOXES`、`/LOG` 和 `/DELETEDATA`（同时删除数据）。

在 PowerShell 中读取退出码：

```powershell
$p = Start-Process .\LiveListen-Setup-0.1.0-x64.exe -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES' -Wait -PassThru
$p.ExitCode
```

`-Wait` 会等待安装程序启动的所有子进程；配合 `/UPDATE` 重新启动程序时，请改用 `$p.WaitForExit()`。

## 日志

安装和卸载总会写日志，默认位于临时文件夹：`%TEMP%\LiveListen-Setup-YYYYMMDD-HHMMSS.log` 或 `LiveListen-Uninstall-YYYYMMDD-HHMMSS.log`。失败页面会显示日志路径。日志只记录步骤、文件数量和错误，不记录识别文字或密钥。

## 便携包

不想安装时，可使用便携包：

```powershell
Expand-Archive -LiteralPath .\LiveListen-0.1.0-portable-x64.zip -DestinationPath .\LiveListen-0.1.0
.\LiveListen-0.1.0\LiveListen\LiveListen.exe
```

- 完整解压到一个新的普通文件夹，不要只复制 EXE，也不要在压缩包内运行。
- 便携指程序免安装，设置和卡片仍保存在 `%APPDATA%\LiveListen`。
- 更新时先从托盘「退出」，再把新版本解压到另一个文件夹；保留旧文件夹便于回退。
- 卸载时先关闭开机启动并退出，再删除解压的文件夹；设置、卡片和凭据默认保留。

## 首次使用

1. 从托盘打开设置，选择麦克风、写入方式和识别服务。
2. 内置豆包兼容通道无需配置，但依赖网页服务行为，可能因服务变化不可用；官方接口需要自己的凭据，见 [官方识别服务](OFFICIAL_SPEECH_PROVIDERS.md)。
3. 服务「检查」只检查本地配置和桥接进程，不代表云端鉴权或识别成功。
4. 在空白文本框中按住右 <kbd>Ctrl</kbd> 说话，松开结束。更多操作见 [使用说明](USAGE.md)。

## 数据位置

| 内容 | 位置 |
| --- | --- |
| 程序 | `%LOCALAPPDATA%\Programs\LiveListen`（或自选位置） |
| 设置与卡片 | `%APPDATA%\LiveListen` |
| 诊断日志 | `%LOCALAPPDATA%\LiveListen\Logs` |
| 官方服务凭据 | Windows 凭据管理器 |
| 安装记录 | 安装目录中的 `install-record.json`，供升级与卸载使用 |

安装包内不含任何用户数据、凭据、会话、录音或开发日志。
