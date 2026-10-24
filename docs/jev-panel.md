# 类型化决策面板

面板支持手动 Choice 分类，以及默认关闭的 TypeSafe Jev 自动评估 Codex 请求。分类结果只作观察，不改变 Codex 的模型或请求路由。TypeSafe 与 OpenCode 的 Jev 都走原生 System One API；其他提供方走 CodexBox 自带的轻量 Rust 适配器，只模拟 Choice 接口的工作方式，不运行 TypeSafe 的 Python Adapter，也不把普通模型的结果称为 Jev 原模型推理。

## 自动评估 Codex

- 在正式 macOS 构建中保存 TypeSafe 官方 Key 后，可开启“自动评估 Codex 请求”。设置持久保存，默认关闭；关闭不要求 Key 仍然可用。自动评估只使用固定 TypeSafe 官方接口和 `jev-latest`，面板选择其他提供方不会改变自动模式。
- 仅在 CodexBox 运行并优化 Codex 时，现有语义 hook 将已解码请求中的最新用户文本交给后台分类。开启即同意向 TypeSafe 发送这些文本，可能产生费用。后台分类不等待 Codex 响应，不改变上游请求；失败不阻止 Codex，退出或旁路后停止后续分类。
- 自动模式跳过空文本、超过 16 KiB 的文本和已满队列中的新任务，不截断后发送。只提取用户文本，不提取图片、系统提示或工具输出；同一请求上下文重复出现的用户文本会去重。
- 自动记录只保存结果、时间、延迟和提供方返回的 token 用量等元数据，不保存用户原文或 Key。最近 200 条自动历史与本次运行的手动记录合并，面板以“Codex 自动”或“手动”标明来源。自动开启时面板每 4 秒刷新；刷新只读记录，不发起分类请求。

## 接入范围

- 预设：TypeSafe、DeepSeek、火山引擎、MiniMax CN/Global、BigModel、阿里云百炼、Xiaomi MiMo、硅基流动、Z.ai、OpenRouter、Kimi CN/Global、BytePlus、AWS Bedrock Mantle、腾讯云 TokenHub、模力方舟 ModelScope、PPIO、Anthropic、Google Gemini、OpenCode Go、OpenCode Jev。OpenAI 不再作为 Jev 面板的内置提供方；兼容协议名称不代表需要 OpenAI Key。所有内置推理地址由后端固定，不能由前端改写。
- 模型目录：TypeSafe 固定 `jev-latest`。有已核实官方模型目录接口的平台，在配置本平台 Key 后读取；OpenRouter 与 OpenCode Go 可读取公开目录。目录请求不发送待分类文本，不使用 TypeSafe Key。请求失败时显示离线预设或允许手填模型 ID。阿里云推理和模型目录都需要 Workspace ID；推理走 `/compatible-mode/v1/chat/completions`，目录走 `/api/v1/models` 并按官方分页读取。BigModel、Z.ai、BytePlus、ModelScope、PPIO 没有在本实现中核实到可自动获取完整模型目录的接口，因此不会标成“已同步全部模型”；它们支持填写官网或账户给出的模型 ID。
- 地区和专属 ID：阿里云、AWS、BytePlus、腾讯云可选地区；火山引擎和 BytePlus 可把账户的 Endpoint ID 作为模型 ID。不同地区或 Workspace 的 Key 绑定不同的固定官方地址；选择或展示模型不会自动发送推理请求。下拉中的模型是否对当前账户开放，以平台实际响应为准。
- OpenCode Go：可选官方 Chat Completions (`/zen/go/v1/chat/completions`) 或 Messages (`/zen/go/v1/messages`) 地址，分别默认选择 `deepseek-v4.1-flash`、`minimax-m3`；两种协议的 Key 不跨地址自动复用。Go 的公开目录不提供可靠的逐模型协议字段，因此在线更新只显示已核实属于当前协议的模型。使用 Go 独立 API Key，不是 OpenCode Zen Key；发送时标识 `CodexBox` User-Agent 和本次单次评估的 UUID 会话。Go 面向编码代理；本面板只提供手动单次分类，不会伪装成 OpenCode。Responses-only 模型未接入。
- OpenCode Jev（原生）：使用 OpenCode Console API Key 和固定的 `https://opencode.ai/inference/systemone/v1/systemone`，模型可选 `jev-1.13` / `jev-1.13-free`。请先在控制台启用所选模型。复用原生 System One 的 `state` / `questions` 请求与带置信度的类型化答案，不发送聊天格式；Key 仅在本次会话内保存，并与 OpenCode Go 分开。
- 自定义：OpenAI Chat Completions 兼容接口或 Anthropic Messages 兼容接口。可填写**完整请求 URL**，也可填写基础地址，由面板按协议拼接 `/chat/completions` 或 `/messages`；发送前会展示最终地址。再填模型 ID 和 API Key。本机 `localhost`、`127.0.0.1` 或 `[::1]` 的 HTTP 接口可以不填 Key；远程地址必须是 HTTPS。URL 不接受用户名、密码、查询参数或片段。专有 API 方言可能需要另外适配。
- TypeSafe：固定 `https://api.typesafe.ai/v1/systemone` 和 `jev-latest`，与兼容模型的 Key 相互隔离。

## 边界与安全

- 不新增监听端口、后台守护进程或全局环境变量。自动模式复用现有 Codex 语义 hook；手动模式只有明确点击“发送并评估”才发出推理请求。选择目标或刷新记录不会发出推理请求。
- 同意框明确展示实际目标 URL。更换提供方、自定义协议或 URL 会清除当前同意；后端将自定义 Key 绑定到确切 URL 和协议，旧 Key 不会发送到新 URL。
- TypeSafe 官方 Key 仅在正式 macOS 构建中保存在登录钥匙串，应用重启后自动恢复，点击“断开”会删除该凭据；Key 不返回前端，也不写入普通配置或日志。调试构建及其他系统当前仍只在本次运行的后端内存保存，不走项目现有的文件回退存储。其他提供方 Key 也仍只保留在本次运行期间，退出后需要重新输入；不同提供方 Key 分开。
- 钥匙串操作在后台线程执行，等待系统授权时不阻塞界面。自动分类 helper 禁止弹出系统授权窗口；尚未授权或钥匙串锁定时记录读取失败并结束，不阻止 Codex。本地重新签名后的应用可能需要用户在 macOS 中重新授权。
- 不自动重试；超时后的远端结果/计费可能未知。连接超时、总超时、输入和响应体、记录数量均有上限；HTTP 重定向关闭。
- 兼容模型只返回经校验的 `coding`、`research`、`writing` 或 `other`；它们的 confidence 一律为空，不冒充经校准置信度。

## 指标范围

请求数、成功率、p50/p95 延迟和提供方返回的 token 用量统计当前筛选范围内最近 200 条记录，包含已保存的自动历史与本次手动记录；兼容提供方按当前请求地址筛选，因此不同地区或 Workspace 不混算。缺少用量时显示缺失，不当作零。

TypeSafe 官方原生 Jev 1.13（`jev-1.13.0`）、Jev 1.14（`jev-1.14.0`）及当前 `jev-latest`、`jev-preview` 的费用估值只使用已上报的输入 token，按每百万输入 token **$0.042** 计算，输出免费。服务端会将请求别名解析为具体版本，费用统计同时识别以上具体版本与别名。未上报用量或没有核实价格的版本不计价，面板明确显示已计价记录数。估值未扣免费额度或折扣，不是账单金额；实际扣款以 TypeSafe 为准。价格与当前别名映射核对于 2026-10-24：[官方模型与价格](https://docs.typesafe.ai/models)、[官方价格说明](https://typesafe.ai/blog/jack-jill-jev-case-study)。其他提供方不套用此单价。

质量没有独立评测，confidence 不代表准确率；分类本身没有压缩前后基线，因此不宣称节省。首页 Headroom 金额是按模型 API 单价计算的 **API 等值估算**，未扣除 Jev 推理成本，也不代表 ChatGPT 或 Claude 套餐账单的实际减免；历史金额不因本次功能重算。

## 手动验证

1. 在 Jev 面板选择提供方与地区；阿里云还需 Workspace ID。设置该提供方 Key 后，从官方目录下拉选择模型；没有可用目录时填写官网模型 ID 或账户专属 Endpoint ID。不要把 Key 粘贴到聊天里。
2. 先用非敏感示例，确认同意框中的完整目标 URL，再点击发送。
3. 查看 Choice、延迟、用量和错误状态；切换自定义 URL 后确认旧 Key 不再使新地址可发送。
4. 在正式 macOS 构建中保存 TypeSafe Key 后重启应用，确认显示“已配置 Key”；点击“断开”后再次重启，确认显示“未配置 Key”。其他构建和其他提供方退出应用后 Key 清除。
5. 开启自动评估后发出一条新的 Codex 请求，确认 Codex 正常响应，面板刷新出现“Codex 自动”记录；关闭开关后，新请求不再产生自动分类。退出或旁路时也应停止后续分类，Codex 继续运行。
