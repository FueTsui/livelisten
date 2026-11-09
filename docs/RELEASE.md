# 构建与发布验证

正式版本和下载文件见 [GitHub Releases](https://github.com/FueTsui/livelisten/releases)。打包脚本只构建本地文件。

## 构建

需要Rust/MSVC、.NET 10 SDK、Python 3.11+、PowerShell。版本均为 0.1.3；下载输入固定在 `scripts/release-dependencies.json`，按Python.org/PyPI元数据校验SHA256，不复制本机site-packages。

```powershell
.\scripts\package-portable.ps1
```

脚本创建新的 `target/release-check/publish-*` 自包含.NET/Windows App SDK输出，以及独立 `rust-static-*` 和 `package-*` 目录。Rust核心与CLI使用相同功能源码，以 `-C target-feature=+crt-static` 链接，避免要求系统另外安装VC运行库；不替换已有普通Release或运行实例。包装前检查实际PE导入表。拒绝已有输出目录，不递归删除或覆盖。

已有完整publish时：

```powershell
.\scripts\package-portable.ps1 -SkipBuild -PublishDirectory .\target\release-check\publish-example -CoreDirectory .\target\release-check\rust-static-example\x86_64-pc-windows-msvc\release -OutputDirectory .\target\release-check\package-example
```

`-SkipBuild`仍要求明确指定已构建的静态CRT核心目录。可用 `-Revision` 记录实际Git提交SHA；没有提交时明确记为 `uncommitted-working-tree`。输出包含完整程序目录、ZIP、逐文件manifest、依赖/许可清单、含核心导入表和SHA256的provenance，以及SHA256SUMS。状态 `built` 表示构建完成。签名状态另行记录。

## 包装边界

- 保留完整.NET、Windows App SDK和Rust核心，校验coreclr/hostfxr/hostpolicy/WinUI存在，不只提供EXE。
- 附带隔离 `runtime/python/python.exe`、固定websocket-client及MIT豆包实时子集；不加载用户site-packages或PYTHONPATH。
- 仅打包用户说明和许可，不放开发记录、PDB、录音或用户配置；拒绝.env、私钥、会话、日志和符号链接输入。
- NuGet/Cargo依赖清单包括构建时工具，不是完整CycloneDX SBOM；全部组件须有原文许可，NuGet只提供SDK许可URL时使用固定的官方原文副本。
- .NET自包含runtime pack的许可从解析的NuGet包收集；发布包未附许可文本的crate（`dasp_sample`，以及安装程序带入的egui系列、`clipboard-win`、`gl_generator`、`khronos_api`、`profiling`）使用 `third_party/licenses/` 下的固定副本，打包时校验SHA-256；来源见各目录的 `UPSTREAM.md`。

## 发布验证

检查固定下载SHA256、归档路径、嵌入式Python版本/导入、最终ZIP CRC与逐文件manifest，再做离线桥接和原生启动。全新Windows、真实设备/云授权、全局热键、输入法及外部文字写入须另行验收，不能由构建成功代替。

安装包 `LiveListen-Setup-<版本>-x64.exe` 由 `setup` crate 的 `pack` 生成，用法与参数见 [INSTALLATION.md](INSTALLATION.md)，设计见 [ADR 0005](adr/0005-preconnect-levels-and-installer.md)。打包脚本运行 `pack` 后检查安装包可通过自身校验、版本与 SHA-256 一致，并把安装包写入发布清单与 SHA256SUMS。每次发布从指定提交构建，核对安装包 `/VERIFY /VERYSILENT`、ZIP 和 SHA-256，再将安装包、便携包、校验值、发布清单和构建来源记录附到同一标签。v0.1.0 为未签名构建。

在只含待公开文件的源码快照上执行凭据扫描和构建检查。下载后可使用发布的 SHA-256 校验值核对文件。
