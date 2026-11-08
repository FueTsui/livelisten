# 官方实时语音服务

2026-11-07：新增 `providers/official_live.py`，通过已有 provider v1 stdio 协议供 Rust 内核调用。保留内置豆包自动发现和旧 `command/args/secretEnv` 配置。依赖沿用 Python 3 与 `websocket-client`，没有新增 Rust 依赖。

## 配置及凭据

`services.customSpeech[]` 的新条目使用 `providerType`，值为 `doubao`、`openai`、`qwen` 或 `tencent`。名称、模型、官方地址及资源 ID 等非秘密字段保存在设置中。缺失类型或类型为 `command` 时仍使用旧 stdio 服务。

| 服务 | 默认模型 / 资源 | 地址 | Windows 凭据 `speech.<id>` 的 JSON 字段 |
| --- | --- | --- | --- |
| 豆包官方 | `bigmodel` / `volc.bigasr.sauc.duration` | `wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async` | `apiKey`；兼容已有 `accessKey` 配合非秘密 `appId` |
| OpenAI | `gpt-live-transcribe` | `wss://api.openai.com/v1/realtime?intent=transcription` | `apiKey` |
| 千问 | `qwen3-asr-flash-realtime` | 北京 `wss://dashscope.aliyuncs.com/api-ws/v1/realtime`；新加坡 `wss://dashscope-intl.aliyuncs.com/api-ws/v1/realtime` | `apiKey` |
| 腾讯 | `16k_zh` | `wss://asr.cloud.tencent.com/asr/v2/`，运行时附加 `appId` 一次 | `secretId`、`secretKey`；`appId` 在设置中 |

凭据仅通过子进程环境传入，不写入设置、参数、重启比较字符串或诊断。桥接启动后从自己的环境删除凭据变量。只允许上述官方主机及相应路径，禁止 WebSocket 重定向。腾讯按其官方协议在运行时生成签名查询，原始 SecretKey 不进入 URL；URL、签名及请求头均不写日志。远程异常及服务错误只映射为预定义错误码，避免服务响应回显凭据。

保存或删除凭据成功后，匹配服务的进程缓存失效；空闲进程释放，活动会话不在保存操作中被打断，下次启动重新读取凭据。诊断只包含凭据是否存在的布尔状态。

“检查配置”只检查本地配置、Python、依赖和本地 stdio 协议握手，不验证账号授权或余额。开启“提前连接”后，选中服务会另行建立官方连接并完成鉴权/配置，不发送音频；关闭此开关后，只在实际 `start` 时连接。OpenAI 已完成下述独立鉴权与合成语音实测；应用内保存的凭据、真实麦克风和其他三家服务仍需分别验证。

## 协议处理

- 豆包：独立二进制头、大端长度、gzip JSON/PCM；API Key 请求头和资源 ID；以服务端最终标志结束，按 `definite` 前缀输出稳定文字。
- OpenAI：`session.update` 的 transcription 模式；将内核的 16 kHz PCM 连续重采样至 24 kHz；接收实时 delta，停止时 commit，并等待 completed。`gpt-live-transcribe` 使用 `languages`，不发送不受支持的服务端 VAD。少于 100 ms 的音频不提交空/过短 buffer。
- 千问：URL 的模型参数、Bearer 认证、16 kHz PCM、服务端 VAD；合并 `text` 和 `stash`，按 item 维护最终结果；停止用 `session.finish` 并等待 `session.finished`，避免丢尾句。
- 腾讯：排序参数、HMAC-SHA1/Base64/URL 编码签名；16 kHz PCM，200 ms 分包并限制为实时速率；按句索引更新结果，停止发送 `end` 并等待 `final=1`。
- 取消会关闭当前传输；连接/服务失败不会伪造成功。停止等待最终结果超时会明确报错。音频队列满会发出带会话 ID 的错误，然后取消该会话，避免内核仍停留在听写状态。
- 每个桥接最多排队或运行 8 个会话；超限的新会话明确失败，避免慢服务与腾讯实时发送速率造成无限积压。内核的整体收尾期限仍然生效，桥接自己的等待上限不会延长内核期限。

## 离线验证

以下检查均使用合成音频或合成凭据，没有读取真实凭据、访问识别服务或使用麦克风：

```powershell
python -B -m unittest discover -s providers/tests -p test_official_live.py -v
cargo test --locked --offline -p live-listen-core config::tests
cargo test --locked --offline -p live-listen-core provider::tests
cargo test --locked --offline -p live-listen-core engine::tests::credential_changes_invalidate_only_matching_service_without_ending_active_session
```

验证结果：Python 13 项、Rust 配置 10 项、provider 10 项、凭据缓存定向测试 1 项全部通过，无忽略项；三个改动 Rust 文件的 rustfmt 检查通过。Python 测试覆盖四种协议完整会话、认证格式、腾讯签名、豆包二进制包、OpenAI 重采样、文字修订及顺序、取消后新会话、断连、服务错误脱敏、最终结果超时、音频/会话队列溢出的会话错误。Rust 测试覆盖旧配置兼容、新字段序列化和边界、官方/旧协议路由、凭据不进入参数和比较字符串、修改凭据的缓存失效，以及四种官方服务的本地 hello/shutdown 进程检查。

本轮记录保存在 `target/voice-next-check/config-tests.txt`、`provider-tests.txt`、`official-tests.txt`、`credential-cache-test.txt`。修改前备份及 SHA256 清单在 `target/voice-next-check/providers-backup/manifest.json`，三份备份的哈希复验一致。

## 官方资料

- [OpenAI 实时转写](https://developers.openai.com/api/docs/guides/realtime-transcription)：当前推荐 `gpt-live-transcribe`、24 kHz 格式、实时 delta 与手动 commit、`languages` 参数及 VAD 限制。
- [千问客户端事件](https://help.aliyun.com/zh/model-studio/qwen-asr-realtime-client-events)、[服务端事件](https://help.aliyun.com/zh/model-studio/qwen-asr-realtime-server-events)、[交互流程](https://help.aliyun.com/zh/model-studio/qwen-asr-realtime-interaction-process)、[实时识别指南](https://help.aliyun.com/zh/model-studio/real-time-speech-recognition-user-guide)。
- [腾讯实时语音识别 WebSocket API](https://cloud.tencent.com/document/product/1093/48982)：认证签名、音频分包与速率、结果索引及结束消息。
- [豆包大模型流式识别](https://docs.volcengine.com/docs/doubao-voice/large-model-streaming-automatic-speech-recognition-api-1?lang=zh)、[双向流式 WebSocket](https://docs.volcengine.com/docs/doubao-voice/bidirectional-streaming-automatic-speech-recognition-websocket?lang=zh)、[API Key 使用](https://docs.volcengine.com/docs/doubao-voice/api-key-usage?lang=en)。

## 提前连接档位

原来的“提前连接”开关已改为三档（设置字段 `dictation.preconnectLevel`）。上文“10 分钟待机窗口”对应“积极”档；各家桥接处理 `prepare` 的方式不变，档位只决定内核何时、持续多久发送 `prepare`：

| 档位 | 启动后准备 | 每次听写后保持就绪 | 无输入多久后不再续期 |
| --- | --- | --- | --- |
| 关闭（`off`，默认） | 否 | 不准备 | — |
| 均衡（`balanced`） | 否 | 2 分钟 | 1 分钟 |
| 积极（`aggressive`） | 是，10 分钟 | 10 分钟 | 3 分钟 |

两档在按住听写键时都会立即开始准备连接。设置从版本 2 升级时，原开关为开的转为“积极”，为关的保持“关闭”。取舍见 [ADR 0005](adr/0005-preconnect-levels-and-installer.md)。
