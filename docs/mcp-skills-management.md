# MCP 与 Skills 管理

本版借鉴 CC Switch 的独立菜单、列表搜索、安装与发现分区，以及操作结果反馈。管理范围是 **Codex 的第三方和自定义扩展**；Codex 自带技能与官方插件市场条目由 Codex 管理，不加入本应用的内置目录和推荐预设。

侧栏提供三个入口：

- **MCP**：管理 Codex 中已有的 MCP 配置，以及新增、编辑、导入自定义服务器。
- **Skills**：通过“已安装 / 发现”标签页查看、搜索、启停和导入技能。
- **工具**：管理 RTK、MarkItDown 等本地工具；推荐预设仍放在这里，并继续适用于既有的受管 MCP 与 Skills 工具包。

第三方受管工具包支持安装、更新、启停和卸载。受管 MCP 卡片位于 MCP 页面下方，受管 Skills 卡片位于“已安装”页下方。自定义 MCP 和导入的技能不加入推荐预设。官方市场中的 Superpowers、Codex Security 不再作为本应用的目录项，也不会因目录清理而卸载。

可编辑的 MCP 与已安装 Skills 使用 Codex 图标开关：启用时显示软件强调色，停用时变灰，并显示文字状态。点击后写入 Codex 配置，写入成功才改变显示；处理中不可重复点击，失败保留原状态。

## MCP：查看、添加与编辑

MCP 页面自动识别并读取 Codex 的全局 `config.toml`，无需设置目录。默认位置为 `~/.codex/config.toml`；设置了 `CODEX_HOME` 时使用该目录。搜索框按服务器名称、命令或 URL 在当前列表中筛选，不会启动服务器。

点击“添加服务器”，填写名称并选择连接方式：

| 连接方式 | 配置内容 |
| --- | --- |
| **stdio** | 启动命令、JSON 数组形式的参数、环境变量值，以及需要从运行环境转发的环境变量名称 |
| **HTTP** | HTTP/HTTPS URL、请求头，以及 Bearer Token 对应的环境变量名称 |

保存后可以编辑、启用、停用或删除自定义服务器；删除前会要求确认。停用保留服务器参数，只修改启用状态。编辑会保留当前表单没有管理的扩展字段及其他 Codex 配置。

Serena、Context7、Codebase Memory 等保留名称，以及当前编辑器无法安全处理的配置形状，在列表中只读显示。保留名称不代表应用拥有该配置：受管包使用原工具卡片操作，外部管理的同名配置应在原管理工具或 Codex 配置中调整。

### 凭据保存位置

填写在环境变量值或 HTTP 请求头中的密钥会保存在 **Codex 配置文件中，不会存入 macOS Keychain**。编辑器会遮蔽这些值，也不会把原始值放进操作错误信息。stdio 的环境变量转发和 HTTP 的 Bearer Token 环境变量字段只保存变量名称，使用前需要在 Codex 的运行环境中提供对应变量。OAuth 登录仍由 Codex 处理。

添加、保存和导入操作只管理配置，不会执行 MCP 命令或发起服务器连接测试。

## MCP：导入 JSON

点击“导入 JSON”，粘贴包含顶层 `mcpServers` 对象的配置，例如：

```json
{
  "mcpServers": {
    "my-server": {
      "url": "https://example.invalid/mcp"
    }
  }
}
```

示例 URL 是占位地址，使用时替换为服务器提供的真实地址。

导入预览仅显示服务器名称、连接方式和同名冲突，不展示环境变量或请求头中的凭据。已有同名条目时，先修改待导入名称再继续。

**MCP 导入采用整批校验：任一名称冲突、重复 JSON 键或配置项校验失败，整批取消，不覆盖已有配置。** 失败后保留输入；成功后显示导入数量及 Codex 的生效步骤。

## Skills：自动发现与管理

打开 Skills 页面便会自动扫描 Codex 的技能位置，不需要填写项目目录或安装目录。已安装列表支持搜索、查看 `SKILL.md` 和启停；现有受管插件仍使用下方工具包卡片管理。

自动扫描用户的 `.agents/skills`、`CODEX_HOME/skills`（默认 `.codex/skills`）和 `/etc/codex/skills`，排除 Codex 自带的 `.system` 技能。扫描会跳过 `.git`、`node_modules`、`target` 等目录，不汇总所有插件缓存。扫描限制为最多 8 层、10,000 个条目和 500 个技能，达到限制时显示提示。

启用或停用会在 Codex 的用户配置 `[[skills.config]]` 中记录技能路径与启用状态，**不会移动、删除或改写技能文件**。

## Skills：点选安装

“发现”页提供公开仓库和本地文件夹两种来源，不预置或自动下载官方技能库。填写无需登录的公开 HTTPS Git 仓库地址并加载后，应用自动列出包含 `SKILL.md` 的技能。搜索名称或描述后点击“安装”，应用会自动找到技能子目录并安装到用户的 `.agents/skills`，不需要填写目录。当前不通过此入口处理私有仓库或凭据登录。

本地导入使用“选择文件夹”：通过系统窗口选择已有技能或技能仓库，应用随即扫描。勾选发现的技能后点击导入，安装位置同样自动确定。来源路径只读展示，不作为需要配置的输入项。

导入要求包含 UTF-8 `SKILL.md`，以闭合的 YAML frontmatter 开头，并具有非空 `name` 和 `description`。发现和导入只读取、复制文件，不执行技能中的脚本。

同名目标目录已存在时不会覆盖。批量本地导入会报告成功和失败数量，并保留失败项的选择。扫描可以跟随技能目录链接并去重，导入目录中的符号链接会被拒绝。

## 保存后如何生效

成功提示表示配置或技能文件已保存。已运行的 Codex 不会由此页面自动刷新工具目录：

- **MCP**：在 Codex 中使用其支持的重载方式，或重启 Codex。
- **Skills**：开启新的 Codex 会话使用技能变更；如客户端仍未读取更新，重启后再检查。

MCP 和 Skills 的配置编辑共用保留注释的 TOML 写入入口，写入前备份并检测期间发生的配置变化，最后原子保存。解析或读取失败时不会用空配置覆盖原文件；原有配置文件符号链接也会保留。这些管理操作与模型代理的启停独立。

## 官方参考

- [Codex MCP 配置](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)
- [Codex Skills](https://learn.chatgpt.com/docs/build-skills)
- [CC Switch 官方 MCP 用户手册](https://github.com/farion1231/cc-switch/blob/main/docs/user-manual/en/3-extensions/3.1-mcp.md)
- [CC Switch 官方 Skills 用户手册](https://github.com/farion1231/cc-switch/blob/main/docs/user-manual/en/3-extensions/3.3-skills.md)
