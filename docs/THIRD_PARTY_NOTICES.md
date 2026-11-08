# 第三方组件

Live Listen自身为专有软件：Copyright © 2026 FueTsui. All Rights Reserved. 这不改变第三方代码和运行库的许可。

完整便携包包含第三方运行文件；此前“没有第三方二进制”的开发描述已不适用。包内 `dependency-inventory.json` 列出解析的依赖，`licenses/` 保留可获得的许可/NOTICE。清单包含构建时组件，不表示每项都在运行时使用。

| 组件 | 许可/来源 | 保留方式 |
| --- | --- | --- |
| Rust依赖 | Cargo.lock固定版本；各包许可元数据和原文；发布包未附原文的使用 `third_party/licenses/` 固定副本 | `licenses/rust/`及清单 |
| egui / eframe 0.31.1（安装程序界面） | MIT 或 Apache-2.0（选用 Apache-2.0）；内嵌字体为 SIL OFL 1.1、Ubuntu Font Licence 1.0 和 MIT | `licenses/rust/epaint_default_fonts-0.31.1/` 等 |
| .NET运行时 | Microsoft官方发行许可及第三方通知 | `licenses/nuget/Microsoft.NETCore.App.Runtime.win-x64-*`，不以项目许可覆盖 |
| Windows App SDK/WinUI/NuGet组件 | 各官方包自带许可与NOTICE；SDK许可从包声明的Microsoft链接取得 | `licenses/nuget/`；包含解析出的runtime pack及构建工具许可 |
| CPython 3.13.17嵌入式x64 | PSF及随附第三方条款；[官方发行页](https://www.python.org/downloads/release/python-31317/) | 原发行包许可位于 `runtime/python/` |
| websocket-client 1.8.0 | Apache-2.0；[官方PyPI条目](https://pypi.org/project/websocket-client/1.8.0/) | wheel的LICENSE和元数据保留在site-packages |
| doubao_asr实时子集 | MIT；Copyright (c) 2026 kuxiaowo | `doubao_asr/LICENSE`和`UPSTREAM.md` |

豆包子集只包含实时桥接需要的三个源码文件，没有原模块其他命令行功能、测试、录音或会话。代码许可不保证在线服务可用；服务仍按提供商账号、授权和政策运行。

本版不随附声纹/翻译模型、sherpa-onnx或3D-Speaker CAM++；Windows系统字体不重新分发。更换依赖时须重新检查许可与安全更新，不能沿用旧清单声称新构建已核验。
