# 指令治理验收记录

## 交付范围

本次完成本地指令分层、四个工具提示模板的修正、Headroom 设置页的只读审计和候选应用/恢复。
没有把两份参考材料全文写进全局配置，也没有进行账号同步、模型切换、工具安装或发布。

## 结构与本地文件

- 全局 `~/.codex/AGENTS.md`：保留已启用工具的短提示；仅刷新 Headroom 托管块。
- 项目 `AGENTS.md`：工程规范入口，引用项目 `CLAUDE.md`，不复制工具手册。
- `CLAUDE.md`：保留项目持久化、兼容性、测试、样式约束，未修改。
- `docs/architecture.md`：修正旧文件引用与 RTK 必需描述，说明所有权边界。
- 详细方法仍属于已有 Skills；权限、模型和原生 Agent 调度属于客户端运行时。

## 模板验收

| 模板 | 生成与适用范围 | 生命周期证据 |
|---|---|---|
| Codex RTK | 路径加引号；原文、机器可读输出和空白检查走 raw；工具不可用时明确回退 | 四模板 round-trip 参数测试 |
| Codex MarkItDown | PDF/Office 文本提取；保留版式、图片、公式验证入口 | 四模板 round-trip 参数测试 |
| Claude Office MarkItDown | Office 文本提取；PDF hook 为条件描述 | 四模板 round-trip 参数测试 |
| Serena | 保留已有可用性、项目选择、简单任务和记忆写入边界 | round-trip 与 Serena 集成测试 |

每个模板验证：准确生成、备份原文、再次应用不改字节且不增加备份、移除后还原用户内容、重复移除无变化。
含空格路径和原文/版式边界有独立回归断言。shell 转义沿用既有 shell_double_quote 和相关 hook 测试。

## 治理能力验收

| 需求 | 实现与证据 |
|---|---|
| 只读预览 | 读取两个客户端的指令，展示当前原文、候选、哈希和托管块状态；预览测试不发生写调用 |
| 不自动启用工具 | 只更新已有且标记完整的托管块；缺失文件保持缺失 |
| 异常标记 | 重复、残缺、嵌套、交叉标记阻止候选修改；嵌套崩溃已有先失败后通过的回归证据 |
| 应用前一致性 | 比对基线和模板哈希，备份后再次核验原文 |
| 备份与恢复 | UUID 快照、原子写入、备份哈希检查；后续编辑或损坏备份阻止覆盖 |
| 错误保护 | 无法备份不修改原文件；符号链接拒绝替换 |
| UI 恢复 | 显示客户端和时间，调用所选持久快照 ID；旧备份缺少时间仍兼容 |
| 本地配置保护 | Codex config.toml 和 Claude settings.json 相对初始基线 SHA-256 未变 |

## 实际验证结果

- `cargo test --manifest-path src-tauri/Cargo.toml --lib client_adapters::tests`：189 passed、1 ignored。
- `cargo test --manifest-path src-tauri/Cargo.toml --lib instruction_governance`：6 passed。
- `npm run test:frontend`：36 个测试文件、522 tests passed，包含三个治理界面测试。
- `npx tsc --noEmit`：通过。
- `cargo check --manifest-path src-tauri/Cargo.toml`：0 errors，120 个既有 dead-code 等警告。
- `cargo build --manifest-path src-tauri/Cargo.toml --example instruction-audit`：通过。
- `git diff --check`：通过。
- 本地 CLI 真正应用后再次审计：所有已有托管模板等于当前模板；不是模拟写入。
- 浅色/深色：真实组件和样式的独立浏览器夹具完成视觉检查；后端数据为模拟值，不宣称原生 Tauri 端到端验证。
- `npm run check:colors`：全局现有硬编码颜色导致失败；本次新增样式专项检查无硬编码颜色。未扩大本次范围去清理旧样式。

## 行为 A/B 的明确限制

T1-T8 协议已建立，结果为 NOT_TESTABLE，未生成虚假的 child/session 证据。
本机 CLI 支持只读、ephemeral 和 JSON 事件；但是还没有在当前执行条件下验证
“保持同等层级、完整工具集合及加载来源”的隔离基线/候选会话。
官方配置文档说明 `developer_instructions` 是开发者层的附加指令，
`project_doc_max_bytes` 是读取 AGENTS.md 的字节上限；这两个配置不能单凭描述
证明与原全局指令加载等价，因此没有用它们替代正式 A/B 后宣称通过。

参考：[OpenAI 配置文档](https://learn.chatgpt.com/docs/config-file/config-reference)。

本次工具模板修复已按用户要求采用；未采用一份未经评测的“全局行为宪章”。
确定性代码与模板测试通过，不代表所有模型、所有任务都不可能出现行为偏差。
原生宿主完整行为 A/B 若作为独立验收目标，需要进一步建立可观测的隔离加载环境。

## 备份和复现

初始证据目录：`.local-test/instruction-governance/20260916-224026/`。
最后同步快照：`a9ada1ad-6416-404c-8c1a-c01d50a0a301`。
快照存放在 Headroom 应用数据目录的 `instruction-snapshots/`，可由设置页或同一 CLI 恢复。

```sh
cargo run --manifest-path src-tauri/Cargo.toml --example instruction-audit
cargo run --manifest-path src-tauri/Cargo.toml --example instruction-audit -- snapshots
```

恢复操作会检查当前文件，拒绝覆盖新编辑。读取/应用逻辑共用客户端模板，未维护第二份模板副本。
修改已在工作区，保留原有未提交工作；本次没有提交、打包、替换已安装应用或发布。
