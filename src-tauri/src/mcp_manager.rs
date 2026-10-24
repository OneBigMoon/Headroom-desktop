//! Codex MCP configuration management. Saving never starts a server.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike, Value};

const RESERVED_IDS: &[&str] = &[
    "serena",
    "context7",
    "codebase-memory",
    "headroom",
    "headroom_local_community",
];
const MANAGED_FIELDS: &[&str] = &[
    "enabled",
    "command",
    "args",
    "env",
    "env_vars",
    "url",
    "bearer_token_env_var",
    "http_headers",
];

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum McpTransport {
    Stdio,
    Http,
    Unsupported,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerInput {
    pub id: String,
    pub transport: McpTransport,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub env_vars: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub bearer_token_env_var: Option<String>,
    #[serde(default)]
    pub http_headers: BTreeMap<String, String>,
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    #[serde(flatten)]
    pub config: McpServerInput,
    pub editable: bool,
    pub managed_by: Option<String>,
    pub readonly_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerSaveRequest {
    pub original_id: Option<String>,
    pub server: McpServerInput,
}

async fn blocking<T, F>(operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|_| "MCP configuration operation did not complete.".to_owned())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn list_mcp_servers() -> Result<Vec<McpServer>, String> {
    blocking(|| list_from_document(&crate::extension_config::read_codex_config()?)).await
}

#[tauri::command]
pub async fn save_mcp_server(request: McpServerSaveRequest) -> Result<Vec<McpServer>, String> {
    blocking(move || {
        crate::extension_config::edit_codex_config(|document| {
            save_to_document(document, request)?;
            list_from_document(document)
        })
    })
    .await
}

#[tauri::command]
pub async fn set_mcp_server_enabled(id: String, enabled: bool) -> Result<Vec<McpServer>, String> {
    blocking(move || {
        crate::extension_config::edit_codex_config(|document| {
            set_enabled_in_document(document, &id, enabled)?;
            list_from_document(document)
        })
    })
    .await
}

#[tauri::command]
pub async fn delete_mcp_server(id: String) -> Result<Vec<McpServer>, String> {
    blocking(move || {
        crate::extension_config::edit_codex_config(|document| {
            delete_from_document(document, &id)?;
            list_from_document(document)
        })
    })
    .await
}

#[tauri::command]
pub async fn import_mcp_servers(json: String) -> Result<Vec<McpServer>, String> {
    blocking(move || {
        let servers = parse_import(&json)?;
        crate::extension_config::edit_codex_config(|document| {
            import_into_document(document, servers)?;
            list_from_document(document)
        })
    })
    .await
}

fn servers(document: &DocumentMut) -> Result<Option<&dyn TableLike>> {
    document
        .get("mcp_servers")
        .map(|item| {
            item.as_table_like()
                .context("The mcp_servers setting must be a TOML table.")
        })
        .transpose()
}

fn servers_mut(document: &mut DocumentMut) -> Result<&mut dyn TableLike> {
    if document.get("mcp_servers").is_none() {
        document["mcp_servers"] = Item::Table(Table::new());
    }
    document["mcp_servers"]
        .as_table_like_mut()
        .context("The mcp_servers setting must be a TOML table.")
}

fn read_string(table: &dyn TableLike, key: &str, supported: &mut bool) -> Option<String> {
    let item = table.get(key)?;
    if let Some(value) = item.as_str() {
        Some(value.to_owned())
    } else {
        *supported = false;
        None
    }
}

fn read_array(table: &dyn TableLike, key: &str, supported: &mut bool) -> Vec<String> {
    let Some(item) = table.get(key) else {
        return Vec::new();
    };
    let values = item.as_array().and_then(|array| {
        array
            .iter()
            .map(|value| value.as_str().map(str::to_owned))
            .collect()
    });
    values.unwrap_or_else(|| {
        *supported = false;
        Vec::new()
    })
}

fn read_map(table: &dyn TableLike, key: &str, supported: &mut bool) -> BTreeMap<String, String> {
    let Some(item) = table.get(key) else {
        return BTreeMap::new();
    };
    let values = item.as_table_like().and_then(|map| {
        map.iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.to_owned(), value.to_owned()))
            })
            .collect()
    });
    values.unwrap_or_else(|| {
        *supported = false;
        BTreeMap::new()
    })
}

fn read_server(id: &str, item: &Item) -> McpServer {
    let empty = Table::new();
    let mut supported = item.as_table_like().is_some();
    let table = item.as_table_like().unwrap_or(&empty);
    let command = read_string(table, "command", &mut supported);
    let url = read_string(table, "url", &mut supported);
    let transport = match (&command, &url) {
        (Some(_), None) => McpTransport::Stdio,
        (None, Some(_)) => McpTransport::Http,
        _ => {
            supported = false;
            McpTransport::Unsupported
        }
    };
    let enabled = table
        .get("enabled")
        .map(|item| {
            item.as_bool().unwrap_or_else(|| {
                supported = false;
                true
            })
        })
        .unwrap_or(true);
    let config = McpServerInput {
        id: id.to_owned(),
        transport,
        enabled,
        command,
        url,
        args: read_array(table, "args", &mut supported),
        env: read_map(table, "env", &mut supported),
        env_vars: read_array(table, "env_vars", &mut supported),
        bearer_token_env_var: read_string(table, "bearer_token_env_var", &mut supported),
        http_headers: read_map(table, "http_headers", &mut supported),
    };
    supported &= validate_advanced_fields(table).is_ok();
    // An ID alone cannot establish who owns an existing registration. Reserve
    // builtin names without adopting or relabelling an external installation.
    let reserved = RESERVED_IDS.contains(&id);
    McpServer {
        config,
        editable: !reserved && supported,
        managed_by: reserved.then(|| "reserved".to_owned()),
        readonly_reason: if reserved {
            Some("This ID is reserved for a built-in tool. Use its existing tool controls, or edit an external registration in the Codex configuration.".to_owned())
        } else if !supported {
            Some("This server uses configuration fields that this editor cannot safely edit. Edit it in the Codex configuration.".to_owned())
        } else {
            None
        },
    }
}

fn list_from_document(document: &DocumentMut) -> Result<Vec<McpServer>> {
    Ok(servers(document)?
        .map(|table| {
            table
                .iter()
                .map(|(id, item)| read_server(id, item))
                .collect()
        })
        .unwrap_or_default())
}

fn require_editable(id: &str, item: &Item) -> Result<()> {
    let server = read_server(id, item);
    if !server.editable {
        bail!("{}", server.readonly_reason.unwrap_or_default());
    }
    Ok(())
}

fn validate_input(input: &McpServerInput) -> Result<()> {
    if input.id.is_empty()
        || input.id.len() > 128
        || !input
            .id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-.".contains(&c))
    {
        bail!("Server ID must contain 1–128 ASCII letters, digits, underscores, hyphens or dots.");
    }
    if RESERVED_IDS.contains(&input.id.as_str()) {
        bail!("This server ID is reserved for a built-in tool. Use its tool controls or the Codex configuration editor.");
    }
    let env_name = |name: &str| {
        let mut bytes = name.bytes();
        bytes
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
            && bytes.all(|c| c.is_ascii_alphanumeric() || c == b'_')
    };
    if input
        .env
        .keys()
        .chain(input.env_vars.iter())
        .any(|name| !env_name(name))
        || input
            .bearer_token_env_var
            .as_ref()
            .is_some_and(|name| !env_name(name))
    {
        bail!("Environment variable names must use letters, digits and underscores, and cannot start with a digit.");
    }
    if input
        .args
        .iter()
        .chain(input.env.values())
        .any(|value| value.contains('\0'))
    {
        bail!("MCP arguments and environment values cannot contain NUL characters.");
    }
    if input.http_headers.iter().any(|(name, value)| {
        name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
            || value.contains(['\r', '\n', '\0'])
    }) {
        bail!("HTTP header names or values are invalid.");
    }
    match input.transport {
        McpTransport::Stdio => {
            if !input
                .command
                .as_ref()
                .is_some_and(|command| !command.trim().is_empty() && !command.contains('\0'))
            {
                bail!("A stdio server requires a command.");
            }
            if input.url.is_some()
                || input.bearer_token_env_var.is_some()
                || !input.http_headers.is_empty()
            {
                bail!("A stdio server cannot contain HTTP connection fields.");
            }
        }
        McpTransport::Http => {
            let url = input
                .url
                .as_deref()
                .context("An HTTP server requires a URL.")?;
            let parsed = reqwest::Url::parse(url)
                .map_err(|_| anyhow::anyhow!("The HTTP server URL is invalid."))?;
            if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                bail!("The server URL must use HTTP or HTTPS and include a host.");
            }
            if input.command.is_some()
                || !input.args.is_empty()
                || !input.env.is_empty()
                || !input.env_vars.is_empty()
            {
                bail!("An HTTP server cannot contain stdio command or environment fields.");
            }
        }
        McpTransport::Unsupported => bail!("Select a supported MCP transport."),
    }
    Ok(())
}

fn string_array(values: &[String]) -> Item {
    let mut array = Array::new();
    for value in values {
        array.push(value.as_str());
    }
    Item::Value(Value::Array(array))
}

fn string_list(item: &Item, choices: Option<&[&str]>) -> bool {
    item.as_array().is_some_and(|array| {
        array.iter().all(|value| {
            value
                .as_str()
                .is_some_and(|value| choices.is_none_or(|choices| choices.contains(&value)))
        })
    })
}

fn string_map_valid(item: &Item) -> bool {
    item.as_table_like()
        .is_some_and(|table| table.iter().all(|(_, value)| value.as_str().is_some()))
}

fn one_of(item: &Item, choices: &[&str]) -> bool {
    item.as_str().is_some_and(|value| choices.contains(&value))
}

const APPROVAL_MODES: &[&str] = &["auto", "prompt", "writes", "approve"];

// Validate known Codex fields while keeping unrecognized future fields verbatim.
// Errors deliberately omit field values, which may contain credentials.
fn validate_advanced_fields(table: &dyn TableLike) -> Result<()> {
    for (key, item) in table.iter() {
        let valid = match key {
            "cwd" | "name" | "oauth_resource" | "environment_id" | "http_headers_helper" => {
                item.as_str().is_some()
            }
            "required" | "supports_parallel_tool_calls" => item.as_bool().is_some(),
            "startup_timeout_sec" | "tool_timeout_sec" => item
                .as_float()
                .or_else(|| item.as_integer().map(|value| value as f64))
                .is_some_and(|value| value.is_finite() && value >= 0.0),
            "startup_timeout_ms" => item.as_integer().is_some_and(|value| value >= 0),
            "tool_input_schema_max_bytes" => item.as_integer().is_some_and(|value| value >= 1),
            "enabled_tools" | "disabled_tools" | "scopes" => string_list(item, None),
            "env_http_headers" => string_map_valid(item),
            "auth" => one_of(item, &["oauth", "chatgpt", "ema_auth"]),
            "default_tools_approval_mode" => one_of(item, APPROVAL_MODES),
            "startup_readiness" => one_of(item, &["connection", "catalog"]),
            "omit_tools_from" => string_list(item, Some(&["code_mode", "deferred", "direct"])),
            "oauth" => item.as_table_like().is_some_and(|oauth| {
                oauth.iter().all(|(key, value)| match key {
                    "authorization_server_issuer"
                    | "callback_url"
                    | "client_id"
                    | "client_secret" => value.as_str().is_some(),
                    "callback_port" => value
                        .as_integer()
                        .is_some_and(|port| (0..=65535).contains(&port)),
                    "token_endpoint_auth_method" => {
                        one_of(value, &["client_secret_basic", "client_secret_post"])
                    }
                    _ => true,
                })
            }),
            "tools" => item.as_table_like().is_some_and(|tools| {
                tools.iter().all(|(_, tool)| {
                    tool.as_table_like().is_some_and(|tool| {
                        tool.iter().all(|(key, value)| match key {
                            "approval_mode" => one_of(value, APPROVAL_MODES),
                            "output_token_limit" => {
                                value.as_integer().is_some_and(|limit| limit >= 1)
                            }
                            _ => true,
                        })
                    })
                })
            }),
            _ => true,
        };
        if !valid {
            bail!("An advanced MCP field has an invalid type or value. No configuration changed.");
        }
    }
    Ok(())
}

fn string_map(values: &BTreeMap<String, String>) -> Item {
    let mut table = InlineTable::new();
    for (key, value) in values {
        table.insert(key, Value::from(value.as_str()));
    }
    Item::Value(Value::InlineTable(table))
}

fn apply_fields(item: &mut Item, input: &McpServerInput) -> Result<()> {
    let mut desired = Table::new();
    desired.insert("enabled", toml_edit::value(input.enabled));
    if let Some(command) = &input.command {
        desired.insert("command", toml_edit::value(command));
    }
    if !input.args.is_empty() {
        desired.insert("args", string_array(&input.args));
    }
    if !input.env.is_empty() {
        desired.insert("env", string_map(&input.env));
    }
    if !input.env_vars.is_empty() {
        desired.insert("env_vars", string_array(&input.env_vars));
    }
    if let Some(url) = &input.url {
        desired.insert("url", toml_edit::value(url));
    }
    if let Some(name) = &input.bearer_token_env_var {
        desired.insert("bearer_token_env_var", toml_edit::value(name));
    }
    if !input.http_headers.is_empty() {
        desired.insert("http_headers", string_map(&input.http_headers));
    }
    let table = item
        .as_table_like_mut()
        .context("The MCP server must be a TOML table.")?;
    for field in MANAGED_FIELDS {
        if let Some(value) = desired.remove(field) {
            update_field(table, field, value);
        } else {
            table.remove(field);
        }
    }
    Ok(())
}

fn update_field(table: &mut dyn TableLike, key: &str, mut value: Item) {
    if let Some(previous) = table.get_mut(key) {
        if let (Some(previous), Some(next)) = (previous.as_array(), value.as_array()) {
            if previous
                .iter()
                .map(Value::as_str)
                .eq(next.iter().map(Value::as_str))
            {
                return;
            }
        }
        if let (Some(previous), Some(next)) = (previous.as_table_like_mut(), value.as_table_like())
        {
            let removed: Vec<_> = previous
                .iter()
                .filter(|(key, _)| !next.contains_key(key))
                .map(|(key, _)| key.to_owned())
                .collect();
            for key in removed {
                previous.remove(&key);
            }
            for (key, value) in next.iter() {
                update_field(previous, key, value.clone());
            }
            return;
        }
        if let (Some(previous), Some(next)) = (previous.as_value(), value.as_value_mut()) {
            *next.decor_mut() = previous.decor().clone();
        }
        // TableLike::insert reformats existing keys and drops their prefix comments.
        *previous = value;
        return;
    }
    table.insert(key, value);
}

fn save_to_document(document: &mut DocumentMut, request: McpServerSaveRequest) -> Result<()> {
    validate_input(&request.server)?;
    let existing = servers(document)?;
    let target = existing.and_then(|table| table.get(&request.server.id));
    if target.is_some() && request.original_id.as_deref() != Some(request.server.id.as_str()) {
        bail!("A server with that ID already exists. No configuration was changed.");
    }
    let mut item = if let Some(original) = &request.original_id {
        let item = existing.and_then(|table| table.get(original)).context(
            "The original MCP server no longer exists. Refresh the list before editing.",
        )?;
        require_editable(original, item)?;
        item.clone()
    } else {
        Item::Table(Table::new())
    };
    apply_fields(&mut item, &request.server)?;
    let table = servers_mut(document)?;
    if let Some(original) = &request.original_id {
        if original != &request.server.id {
            table.remove(original);
        }
    }
    table.insert(&request.server.id, item);
    Ok(())
}

fn set_enabled_in_document(document: &mut DocumentMut, id: &str, enabled: bool) -> Result<()> {
    let item = servers(document)?
        .and_then(|table| table.get(id))
        .context("The MCP server no longer exists.")?;
    require_editable(id, item)?;
    let table = servers_mut(document)?
        .get_mut(id)
        .and_then(Item::as_table_like_mut)
        .context("The MCP server must be a TOML table.")?;
    update_field(table, "enabled", toml_edit::value(enabled));
    Ok(())
}

fn delete_from_document(document: &mut DocumentMut, id: &str) -> Result<()> {
    let item = servers(document)?
        .and_then(|table| table.get(id))
        .context("The MCP server no longer exists.")?;
    require_editable(id, item)?;
    let removed = servers_mut(document)?.remove(id);
    if let Some(table) = removed.as_ref().and_then(Item::as_table) {
        let prefix = table
            .decor()
            .prefix()
            .and_then(|prefix| prefix.as_str())
            .unwrap_or_default();
        if !prefix.is_empty() {
            // A marker preceding a server belongs to the document, not to that server.
            if let Some(next) = table
                .position()
                .and_then(|position| next_table_position(document.as_table(), position))
            {
                prepend_table_prefix(document.as_table_mut(), next, prefix);
            } else {
                let trailing = document.trailing().as_str().unwrap_or_default();
                document.set_trailing(format!("{prefix}{trailing}"));
            }
        }
    }
    Ok(())
}

fn next_table_position(table: &Table, after: isize) -> Option<isize> {
    let mut next = table.position().filter(|position| *position > after);
    for (_, item) in table.iter() {
        let children: Vec<_> = match item {
            Item::Table(table) => vec![table],
            Item::ArrayOfTables(tables) => tables.iter().collect(),
            _ => Vec::new(),
        };
        for child in children {
            if let Some(position) = next_table_position(child, after) {
                next = Some(next.map_or(position, |next| next.min(position)));
            }
        }
    }
    next
}

fn prepend_table_prefix(table: &mut Table, position: isize, prefix: &str) -> bool {
    if table.position() == Some(position) {
        let previous = table
            .decor()
            .prefix()
            .and_then(|prefix| prefix.as_str())
            .unwrap_or_default();
        let combined = format!("{prefix}{previous}");
        table.decor_mut().set_prefix(combined);
        return true;
    }
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(child) => {
                if prepend_table_prefix(child, position, prefix) {
                    return true;
                }
            }
            Item::ArrayOfTables(children) => {
                for child in children.iter_mut() {
                    if prepend_table_prefix(child, position, prefix) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

#[derive(Deserialize)]
struct ImportRoot {
    #[serde(rename = "mcpServers")]
    servers: BTreeMap<String, ImportedServer>,
}

// serde_json::Value normally keeps only the last duplicate key. Reject duplicates
// at every depth before importing, including environment/header maps and unknown
// future fields, so the import never silently selects one of two settings.
struct UniqueJson(serde_json::Value);

impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueJson;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("JSON without duplicate object keys")
            }
            fn visit_bool<E: serde::de::Error>(
                self,
                value: bool,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJson(value.into()))
            }
            fn visit_i64<E: serde::de::Error>(
                self,
                value: i64,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJson(value.into()))
            }
            fn visit_u64<E: serde::de::Error>(
                self,
                value: u64,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJson(value.into()))
            }
            fn visit_f64<E: serde::de::Error>(
                self,
                value: f64,
            ) -> std::result::Result<Self::Value, E> {
                serde_json::Number::from_f64(value)
                    .map(|value| UniqueJson(value.into()))
                    .ok_or_else(|| E::custom("Invalid JSON number."))
            }
            fn visit_str<E: serde::de::Error>(
                self,
                value: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJson(value.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJson(serde_json::Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<UniqueJson>()? {
                    values.push(value.0);
                }
                Ok(UniqueJson(values.into()))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((key, value)) = map.next_entry::<String, UniqueJson>()? {
                    if values.insert(key, value.0).is_some() {
                        return Err(serde::de::Error::custom("Duplicate JSON object key."));
                    }
                }
                Ok(UniqueJson(values.into()))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[derive(Deserialize)]
struct ImportedServer {
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default, alias = "envVars")]
    env_vars: Vec<String>,
    url: Option<String>,
    #[serde(alias = "bearerTokenEnvVar")]
    bearer_token_env_var: Option<String>,
    #[serde(default, alias = "httpHeaders", alias = "headers")]
    http_headers: BTreeMap<String, String>,
    enabled: Option<bool>,
    disabled: Option<bool>,
    #[serde(rename = "type")]
    transport: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

fn json_value(value: serde_json::Value) -> Result<Value> {
    Ok(match value {
        serde_json::Value::Null => {
            bail!("An imported setting contains null, which cannot be stored in TOML.")
        }
        serde_json::Value::Bool(value) => Value::from(value),
        serde_json::Value::String(value) => Value::from(value),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Value::from(value)
            } else if value.is_f64() {
                Value::from(
                    value
                        .as_f64()
                        .context("An imported numeric setting cannot be stored in TOML.")?,
                )
            } else {
                bail!("An imported integer is outside the range supported by TOML.");
            }
        }
        serde_json::Value::Array(values) => {
            let mut array = Array::new();
            for value in values {
                array.push(json_value(value)?);
            }
            Value::Array(array)
        }
        serde_json::Value::Object(values) => {
            let mut table = InlineTable::new();
            for (key, value) in values {
                table.insert(key, json_value(value)?);
            }
            Value::InlineTable(table)
        }
    })
}

fn parse_import(json: &str) -> Result<Vec<(String, Item)>> {
    let invalid_json = || {
        anyhow::anyhow!(
            "Invalid MCP JSON. Use a mcpServers object with unique keys and valid field types."
        )
    };
    let parsed: UniqueJson = serde_json::from_str(json).map_err(|_| invalid_json())?;
    let root: ImportRoot = serde_json::from_value(parsed.0).map_err(|_| invalid_json())?;
    if root.servers.is_empty() {
        bail!("The import contains no MCP servers.");
    }
    root.servers.into_iter().map(|(id, server)| {
        let transport = match (&server.command, &server.url) {
            (Some(_), None) => McpTransport::Stdio,
            (None, Some(_)) => McpTransport::Http,
            _ => bail!("Each imported server must provide either a command or a URL."),
        };
        if let Some(kind) = server.transport.as_deref() {
            let compatible = match transport {
                McpTransport::Stdio => kind == "stdio",
                McpTransport::Http => matches!(kind, "http" | "streamable-http"),
                McpTransport::Unsupported => false,
            };
            if !compatible { bail!("An imported transport is unsupported or conflicts with its connection fields."); }
        }
        if let (Some(enabled), Some(disabled)) = (server.enabled, server.disabled) {
            if enabled == disabled { bail!("An imported server has conflicting enabled and disabled settings."); }
        }
        let input = McpServerInput {
            id: id.clone(), transport,
            enabled: server.enabled.unwrap_or(!server.disabled.unwrap_or(false)),
            command: server.command, args: server.args, env: server.env, env_vars: server.env_vars,
            url: server.url, bearer_token_env_var: server.bearer_token_env_var, http_headers: server.http_headers,
        };
        validate_input(&input)?;
        let mut table = Table::new();
        for (key, value) in server.extra { table.insert(&key, Item::Value(json_value(value)?)); }
        let mut item = Item::Table(table);
        apply_fields(&mut item, &input)?;
        validate_advanced_fields(item.as_table_like().expect("new server table"))?;
        Ok((id, item))
    }).collect()
}

fn import_into_document(document: &mut DocumentMut, imported: Vec<(String, Item)>) -> Result<()> {
    if let Some(existing) = servers(document)? {
        if imported.iter().any(|(id, _)| existing.contains_key(id)) {
            bail!("An imported server ID already exists. The entire import was cancelled; no configuration was changed.");
        }
    }
    let table = servers_mut(document)?;
    for (id, item) in imported {
        table.insert(&id, item);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANAGED_MARKERS: &str = "# >>> headroom-local-community:codex_cli >>>\nmodel_provider = 'headroom_local_community'\n# <<< headroom-local-community:codex_cli <<<\n\n# User server note\n[mcp_servers.existing]\n# User command note\ncommand = 'node' # command suffix\ncustom_future = { nested = true }\n\n# >>> headroom-local-community:codex_cli_provider >>>\n[model_providers.headroom_local_community]\nbase_url = 'http://127.0.0.1:6891/v1'\n# <<< headroom-local-community:codex_cli_provider <<<\n";

    fn assert_managed_markers(saved: &str) {
        for marker in [
            "# >>> headroom-local-community:codex_cli >>>",
            "# <<< headroom-local-community:codex_cli <<<",
            "# >>> headroom-local-community:codex_cli_provider >>>",
            "# <<< headroom-local-community:codex_cli_provider <<<",
        ] {
            assert_eq!(
                saved.matches(marker).count(),
                1,
                "lost or duplicated {marker}"
            );
        }
        assert!(
            saved
                .find("# <<< headroom-local-community:codex_cli <<<")
                .unwrap()
                < saved
                    .find("[model_providers.headroom_local_community]")
                    .unwrap()
        );
        saved.parse::<DocumentMut>().unwrap();
    }

    #[test]
    fn managed_markers_survive_deleting_the_first_mcp_table() {
        let mut document = MANAGED_MARKERS.parse::<DocumentMut>().unwrap();
        delete_from_document(&mut document, "existing").unwrap();
        let saved = document.to_string();
        assert_managed_markers(&saved);
        assert!(saved.contains("# User server note"));
        assert!(!saved.contains("[mcp_servers.existing]"));
    }

    #[test]
    fn managed_markers_and_user_comments_survive_renaming_an_mcp_table() {
        let mut document = MANAGED_MARKERS.parse::<DocumentMut>().unwrap();
        let mut server = list_from_document(&document).unwrap().remove(0).config;
        server.id = "renamed".into();
        save_to_document(
            &mut document,
            McpServerSaveRequest {
                original_id: Some("existing".into()),
                server,
            },
        )
        .unwrap();
        let saved = document.to_string();
        assert_managed_markers(&saved);
        assert!(saved.contains("# User server note"));
        assert!(saved.contains("# User command note"));
        assert!(saved.contains("# command suffix"));
        assert!(saved.contains("custom_future = { nested = true }"));
        assert!(
            saved
                .find("# <<< headroom-local-community:codex_cli <<<")
                .unwrap()
                < saved.find("[mcp_servers.renamed]").unwrap()
        );
    }

    #[test]
    fn managed_markers_survive_delete_before_array_table_or_document_end() {
        let with_skills = MANAGED_MARKERS.replace("# >>> headroom-local-community:codex_cli_provider >>>", "[[skills.config]]\npath = '/fixture/skill/SKILL.md'\nenabled = false\n\n# >>> headroom-local-community:codex_cli_provider >>>");
        let mut document = with_skills.parse::<DocumentMut>().unwrap();
        delete_from_document(&mut document, "existing").unwrap();
        let saved = document.to_string();
        assert_managed_markers(&saved);
        assert!(
            saved
                .find("# <<< headroom-local-community:codex_cli <<<")
                .unwrap()
                < saved.find("[[skills.config]]").unwrap()
        );
        assert!(saved.contains("enabled = false"));

        let last = "# >>> headroom-local-community:codex_cli >>>\nmodel_provider = 'headroom_local_community'\n# <<< headroom-local-community:codex_cli <<<\n\n# >>> headroom-local-community:codex_cli_provider >>>\n[model_providers.headroom_local_community]\nbase_url = 'http://127.0.0.1:6891/v1'\n# <<< headroom-local-community:codex_cli_provider <<<\n\n# Last server note\n[mcp_servers.last]\ncommand = 'node'\n";
        let mut document = last.parse::<DocumentMut>().unwrap();
        delete_from_document(&mut document, "last").unwrap();
        let saved = document.to_string();
        assert_managed_markers(&saved);
        assert!(
            saved
                .find("[model_providers.headroom_local_community]")
                .unwrap()
                < saved
                    .find("# <<< headroom-local-community:codex_cli_provider <<<")
                    .unwrap()
        );
    }

    #[test]
    fn known_advanced_field_types_are_validated_before_any_import() {
        for (field, value) in [
            ("cwd", serde_json::json!(123)),
            ("startup_timeout_sec", serde_json::json!("not-a-number")),
            ("tool_timeout_sec", serde_json::json!(-1)),
            ("startup_timeout_ms", serde_json::json!(1.5)),
            ("required", serde_json::json!("yes")),
            ("enabled_tools", serde_json::json!([1])),
            (
                "env_http_headers",
                serde_json::json!({ "Authorization": 42 }),
            ),
            ("auth", serde_json::json!("unsupported")),
            ("tool_input_schema_max_bytes", serde_json::json!(0)),
            ("oauth", serde_json::json!({ "callback_port": 65536 })),
            (
                "oauth",
                serde_json::json!({ "client_id": ["not-a-string"] }),
            ),
            (
                "tools",
                serde_json::json!({ "query": { "output_token_limit": 0 } }),
            ),
        ] {
            let mut invalid = serde_json::json!({ "command": "node" });
            invalid[field] = value;
            let json = serde_json::json!({ "mcpServers": { "a_valid": { "command": "existing" }, "z_invalid": invalid } }).to_string();
            let mut document = "other = 7\n".parse::<DocumentMut>().unwrap();
            let original = document.to_string();
            let result = parse_import(&json)
                .and_then(|servers| import_into_document(&mut document, servers));
            assert!(result.is_err(), "accepted invalid {field}");
            assert_eq!(document.to_string(), original);
            assert!(!result.unwrap_err().to_string().contains("not-a-string"));
        }
    }

    #[test]
    fn valid_advanced_fields_and_unknown_future_fields_roundtrip() {
        let json = serde_json::json!({ "mcpServers": {
            "local": { "command": "node", "cwd": "/fixture/project", "startup_timeout_sec": 1.5, "tool_timeout_sec": 42, "required": true, "enabled_tools": ["query"], "tools": { "query": { "approval_mode": "prompt", "output_token_limit": 100 } }, "future": { "preserve": true } },
            "remote": { "url": "https://example.invalid/mcp", "auth": "oauth", "env_http_headers": { "Authorization": "MCP_AUTH" }, "oauth": { "client_id": "public-client", "callback_port": 8080 } }
        }}).to_string();
        let mut document = DocumentMut::new();
        import_into_document(&mut document, parse_import(&json).unwrap()).unwrap();
        let saved = document.to_string();
        let document = saved.parse::<DocumentMut>().unwrap();
        assert!(list_from_document(&document)
            .unwrap()
            .iter()
            .all(|server| server.editable));
        assert_eq!(
            document["mcp_servers"]["local"]["cwd"].as_str(),
            Some("/fixture/project")
        );
        assert_eq!(
            document["mcp_servers"]["local"]["future"]["preserve"].as_bool(),
            Some(true)
        );
        assert_eq!(
            document["mcp_servers"]["remote"]["env_http_headers"]["Authorization"].as_str(),
            Some("MCP_AUTH")
        );
    }

    #[test]
    fn existing_invalid_advanced_fields_are_readonly_and_comments_survive_updates() {
        let invalid = "[mcp_servers.invalid]\ncommand = 'node'\ncwd = 123\n"
            .parse::<DocumentMut>()
            .unwrap();
        assert!(!list_from_document(&invalid).unwrap()[0].editable);
        let source = "[mcp_servers.custom]\ncommand = 'node' # command note\nenabled = true # enabled note\nargs = [\n  'server.js', # argument note\n]\n\n# Environment note\n[mcp_servers.custom.env]\nVALUE = 'old' # value note\n";
        let mut document = source.parse::<DocumentMut>().unwrap();
        let mut server = list_from_document(&document).unwrap().remove(0).config;
        server.command = Some("new-command".into());
        server.env.insert("VALUE".into(), "new".into());
        save_to_document(
            &mut document,
            McpServerSaveRequest {
                original_id: Some("custom".into()),
                server,
            },
        )
        .unwrap();
        set_enabled_in_document(&mut document, "custom", false).unwrap();
        let saved = document.to_string();
        for comment in [
            "# command note",
            "# enabled note",
            "# argument note",
            "# Environment note",
            "# value note",
        ] {
            assert!(saved.contains(comment), "lost {comment}");
        }
        let document = saved.parse::<DocumentMut>().unwrap();
        assert_eq!(
            document["mcp_servers"]["custom"]["env"]["VALUE"].as_str(),
            Some("new")
        );
    }

    const FIXTURE: &str = r#"# proxy marker must survive
model_provider = "headroom_local_community"
[model_providers.headroom_local_community]
base_url = "http://127.0.0.1:6891/v1"
[mcp_servers.existing]
command = "node"
args = ["server.js", "--flag"]
env = { TOKEN = "fixture-only" }
env_vars = ["HOME"]
cwd = "/fixture/project"
startup_timeout_sec = 42
custom_future = { nested = true }
"#;

    fn stdio(id: &str) -> McpServerInput {
        McpServerInput {
            id: id.to_owned(),
            transport: McpTransport::Stdio,
            enabled: true,
            command: Some("fixture-command".to_owned()),
            args: vec![],
            env: BTreeMap::new(),
            env_vars: vec![],
            url: None,
            bearer_token_env_var: None,
            http_headers: BTreeMap::new(),
        }
    }

    #[test]
    fn edit_roundtrip_preserves_unknown_fields_proxy_and_comment() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(&path, FIXTURE).unwrap();
        let mut document: DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        let mut input = list_from_document(&document).unwrap().remove(0).config;
        input.id = "renamed".to_owned();
        input.command = Some("new-command".to_owned());
        save_to_document(
            &mut document,
            McpServerSaveRequest {
                original_id: Some("existing".to_owned()),
                server: input,
            },
        )
        .unwrap();
        crate::client_adapters::atomic_write(&path, document.to_string().as_bytes()).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        let reloaded: DocumentMut = saved.parse().unwrap();
        assert!(saved.contains("# proxy marker must survive"));
        assert_eq!(
            reloaded["model_provider"].as_str(),
            Some("headroom_local_community")
        );
        assert_eq!(
            reloaded["model_providers"]["headroom_local_community"]["base_url"].as_str(),
            Some("http://127.0.0.1:6891/v1")
        );
        assert!(reloaded["mcp_servers"].get("existing").is_none());
        let server = &reloaded["mcp_servers"]["renamed"];
        assert_eq!(server["cwd"].as_str(), Some("/fixture/project"));
        assert_eq!(server["startup_timeout_sec"].as_integer(), Some(42));
        assert_eq!(server["custom_future"]["nested"].as_bool(), Some(true));
        assert_eq!(server["env"]["TOKEN"].as_str(), Some("fixture-only"));
    }

    #[test]
    fn disabling_preserves_all_connection_parameters() {
        let mut document: DocumentMut = FIXTURE.parse().unwrap();
        let before = document["mcp_servers"]["existing"].clone();
        set_enabled_in_document(&mut document, "existing", false).unwrap();
        assert_eq!(
            document["mcp_servers"]["existing"]["enabled"].as_bool(),
            Some(false)
        );
        let mut after = document["mcp_servers"]["existing"].clone();
        after.as_table_like_mut().unwrap().remove("enabled");
        assert_eq!(before.to_string(), after.to_string());
    }

    #[test]
    fn save_conflicts_and_missing_original_do_not_mutate() {
        let mut document: DocumentMut = FIXTURE.parse().unwrap();
        let before = document.to_string();
        for request in [
            McpServerSaveRequest {
                original_id: None,
                server: stdio("existing"),
            },
            McpServerSaveRequest {
                original_id: Some("missing".to_owned()),
                server: stdio("new"),
            },
            McpServerSaveRequest {
                original_id: Some("other".to_owned()),
                server: stdio("existing"),
            },
        ] {
            assert!(save_to_document(&mut document, request).is_err());
            assert_eq!(document.to_string(), before);
        }
    }

    #[test]
    fn json_import_supports_stdio_http_and_unknown_fields() {
        let imported = parse_import(r#"{"mcpServers":{
            "local":{"command":"node","args":["local.js"],"env":{"TOKEN":"fixture"},"envVars":["HOME"],"disabled":true,"cwd":"/fixture"},
            "remote":{"type":"http","url":"https://example.invalid/mcp","bearerTokenEnvVar":"MCP_TOKEN","headers":{"X-Fixture":"value"},"oauth":{"scope":"read"}}
        }}"#).unwrap();
        let mut document = DocumentMut::new();
        import_into_document(&mut document, imported).unwrap();
        let listed = list_from_document(&document).unwrap();
        let local = &listed
            .iter()
            .find(|server| server.config.id == "local")
            .unwrap()
            .config;
        assert!(!local.enabled);
        assert_eq!(local.env_vars, ["HOME"]);
        let remote = &listed
            .iter()
            .find(|server| server.config.id == "remote")
            .unwrap()
            .config;
        assert_eq!(remote.transport, McpTransport::Http);
        assert_eq!(
            remote.http_headers.get("X-Fixture").map(String::as_str),
            Some("value")
        );
        assert_eq!(
            document["mcp_servers"]["remote"]["oauth"]["scope"].as_str(),
            Some("read")
        );
        assert_eq!(
            document["mcp_servers"]["local"]["cwd"].as_str(),
            Some("/fixture")
        );
        let mut remote = listed
            .into_iter()
            .find(|server| server.config.id == "remote")
            .unwrap()
            .config;
        remote.url = Some("https://example.invalid/updated".to_owned());
        save_to_document(
            &mut document,
            McpServerSaveRequest {
                original_id: Some("remote".to_owned()),
                server: remote,
            },
        )
        .unwrap();
        assert_eq!(
            document["mcp_servers"]["remote"]["oauth"]["scope"].as_str(),
            Some("read")
        );
    }

    #[test]
    fn import_is_all_or_nothing_for_conflicts_and_duplicate_ids() {
        let mut document: DocumentMut = FIXTURE.parse().unwrap();
        let before = document.to_string();
        let imported = parse_import(
            r#"{"mcpServers":{"new":{"command":"new"},"existing":{"command":"replacement"}}}"#,
        )
        .unwrap();
        assert!(import_into_document(&mut document, imported).is_err());
        assert_eq!(document.to_string(), before);
        assert!(parse_import(
            r#"{"mcpServers":{"same":{"command":"first"},"same":{"command":"second"}}}"#
        )
        .is_err());
        for json in [
            r#"{"mcpServers":{},"mcpServers":{"new":{"command":"new"}}}"#,
            r#"{"mcpServers":{"new":{"command":"node","env":{"TOKEN":"first","TOKEN":"second"}}}}"#,
            r#"{"mcpServers":{"new":{"url":"https://example.invalid","headers":{"X-Test":"first","X-Test":"second"}}}}"#,
            r#"{"mcpServers":{"new":{"command":"node","future":{"nested":1,"nested":2}}}}"#,
            r#"{"mcpServers":{"new":{"command":"node","future":18446744073709551615}}}"#,
        ] {
            assert!(parse_import(json).is_err());
        }
        assert!(parse_import(
            r#"{"mcpServers":{"new":{"command":"new"},"invalid":{"url":"file:///fixture"}}}"#
        )
        .is_err());
        assert_eq!(document.to_string(), before);
    }

    #[test]
    fn builtin_and_complex_env_vars_are_readonly() {
        let mut document: DocumentMut = r#"
[mcp_servers.serena]
command = "serena"
[mcp_servers.complex]
command = "node"
env_vars = [{ name = "TOKEN", source = "secret-manager" }]
"#
        .parse()
        .unwrap();
        let before = document.to_string();
        let listed = list_from_document(&document).unwrap();
        assert!(listed.iter().all(|server| !server.editable));
        assert_eq!(
            listed
                .iter()
                .find(|server| server.config.id == "serena")
                .unwrap()
                .managed_by
                .as_deref(),
            Some("reserved")
        );
        for id in ["serena", "complex"] {
            assert!(set_enabled_in_document(&mut document, id, false).is_err());
            assert!(delete_from_document(&mut document, id).is_err());
            assert!(save_to_document(
                &mut document,
                McpServerSaveRequest {
                    original_id: Some(id.to_owned()),
                    server: stdio(id)
                }
            )
            .is_err());
        }
        assert_eq!(document.to_string(), before);
    }

    #[test]
    fn inline_tables_and_deleting_custom_server_keep_other_settings() {
        let mut document: DocumentMut =
            "other = 7\nmcp_servers = { custom = { command = 'node', future = 9 } }\n"
                .parse()
                .unwrap();
        set_enabled_in_document(&mut document, "custom", false).unwrap();
        save_to_document(
            &mut document,
            McpServerSaveRequest {
                original_id: None,
                server: stdio("new"),
            },
        )
        .unwrap();
        delete_from_document(&mut document, "new").unwrap();
        let document: DocumentMut = document.to_string().parse().unwrap();
        assert_eq!(
            document["mcp_servers"]["custom"]["future"].as_integer(),
            Some(9)
        );
        assert_eq!(document["other"].as_integer(), Some(7));
    }

    #[test]
    fn validation_errors_never_echo_secret_values() {
        let secret = "fixture-secret-should-not-appear";
        let json =
            format!(r#"{{"mcpServers":{{"invalid":{{"command":"node","args":"{secret}"}}}}}}"#);
        assert!(!parse_import(&json)
            .err()
            .unwrap()
            .to_string()
            .contains(secret));
        let mut input = stdio("test");
        input
            .env
            .insert("invalid name".to_owned(), secret.to_owned());
        assert!(!validate_input(&input)
            .err()
            .unwrap()
            .to_string()
            .contains(secret));
        input.env.clear();
        input.command = None;
        input.transport = McpTransport::Http;
        input.url = Some(format!("invalid-{secret}"));
        assert!(!validate_input(&input)
            .err()
            .unwrap()
            .to_string()
            .contains(secret));
        input.url = Some("https://example.invalid/mcp".to_owned());
        input
            .http_headers
            .insert("Authorization".to_owned(), format!("{secret}\r\n"));
        assert!(!validate_input(&input)
            .err()
            .unwrap()
            .to_string()
            .contains(secret));
    }
}
