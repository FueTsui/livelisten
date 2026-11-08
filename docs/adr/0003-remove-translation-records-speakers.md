# 0003 移除翻译、对话记录和说话人区分；卡片可单独设置外观

- 日期：2026-11-07
- 状态：采用

## 背景

产品范围收窄为听写、实时字幕和随手记。字幕翻译、对话记录（含摘要、待办、导出、重新翻译、保留天数）和按声音认说话人带来了一个外部 HTTP 客户端（`ureq` + rustls）、两组接口密钥、本机长期保存的对话文字和声纹特征，以及需要单独下载、没有数字签名的 sherpa-onnx、ONNX Runtime 和声纹模型。用户另外希望桌面卡片既能统一设置外观，也能单独设置。

## 决定

- 删除核心的 `translate`、`record`、`voiceprint`、`export` 模块和对应命令、事件、设置、快捷键、界面页面、文字和构建步骤；删除 `vendor/voiceprint` 和 `scripts/fetch-voiceprint.ps1`。
- 识别服务协议仍是第 1 版：`speaker` 字段和 `speakers` 能力不再属于协议，旧服务发来时忽略，不报错。
- 设置文件版本升到 2。旧字段由 serde 忽略，启动时发现旧版本就立即保存一次；凭据管理器里的 `LiveListen/translation` 和 `LiveListen/summary` 在升级时删除。`secret.*` 只接受 `speech.<服务 ID>`。
- `%APPDATA%\LiveListen\records` 中已有的记录文件不自动删除：那是用户数据，删除无法恢复，由用户决定。
- 卡片的外观：`appearance.cards` 仍是统一外观；`cards.json` 中每张卡片可带可选的 `style`（字段同 `SurfaceStyle`，按相同规则校验）。有 `style` 的卡片用自己的外观，否则用统一外观。`cards.update` 接受 `style`（对象或 `null`），`cards.styles.clear` 一次清除全部。界面在拖动滑块时先在卡片上预览，停顿约 0.3 秒后再保存。

## 后果

- 依赖减少：Cargo.lock 少了 ureq、rustls、ring 等包；发布包不再带第三方 DLL 和模型。
- 不再有外发的文字（只有音频发给识别服务），威胁模型和数据去向表随之缩短。
- 旧版本留下的记录文件在新版本中无法查看；需要时只能用文本编辑器打开 JSON。
- 卡片单独设置外观后，修改统一外观不会影响它，需要用「使用统一外观」或「全部改用统一外观」改回。
