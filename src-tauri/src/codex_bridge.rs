use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const REPOSITORY: &str = "https://github.com/XiaoDuoYa/codex-with-chatgpt.git";
const PACKAGE_MANAGER: &str = "pnpm@11.24.0";
const BRIDGE_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const BRIDGE_SETUP_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexBridgeStatus {
    pub installed: bool,
    pub node_available: bool,
    pub node_version: Option<String>,
    pub source_path: String,
    pub running: bool,
    pub port: Option<u16>,
    pub paired: bool,
    /// Whether the local Codex CLI has an existing ChatGPT OAuth session.
    /// This is diagnostic only; it never substitutes for ChatGPT connector consent.
    pub codex_logged_in: bool,
    pub browser_auth_supported: bool,
    pub message: String,
    pub output: Option<String>,
    /// Non-sensitive workspace identity returned by c2c status.
    pub workspace_name: Option<String>,
    pub workspace_id: Option<String>,
    pub public_url: Option<String>,
    pub tunnel_mode: Option<String>,
    pub tunnel_hostname: Option<String>,
    pub tunnel_stable: bool,
    pub tunnel_fallback_reason: Option<String>,
    pub token_count: Option<u64>,
    pub pairing_active: Option<bool>,
    /// Pairing code is intentionally kept separate from diagnostic output.
    pub pairing_code: Option<String>,
    /// Expiration timestamp (milliseconds since Unix epoch) for the one-time code.
    pub pairing_expires_at: Option<u64>,
    /// URL users should open to complete ChatGPT connector authorization.
    pub authorization_url: Option<String>,
    pub mcp_url: Option<String>,
    pub connector_name: Option<String>,
    pub endpoint_changed: bool,
    pub endpoint_repair_required: bool,
    pub safe_output: Option<String>,
    pub workspace_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairingMetadata {
    pairing_code: String,
    pairing_expires_at: u64,
    mcp_url: Option<String>,
    workspace_id: Option<String>,
    workspace_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct EndpointMetadata {
    public_url: Option<String>,
    mcp_url: Option<String>,
    connector_name: Option<String>,
}

fn root() -> PathBuf {
    dirs::data_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("codex-with-chatgpt")
}

fn source() -> PathBuf {
    root().join("source")
}

fn program_path(program: &str) -> PathBuf {
    if let Some(path) = crate::client_adapters::find_on_path(&[program]) {
        return path;
    }
    for base in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        let path = Path::new(base).join(program);
        if path.exists() {
            return path;
        }
    }
    let nvm = dirs::home_dir().map(|home| home.join(".nvm/versions/node"));
    if let Some(nvm) = nvm {
        if let Ok(entries) = std::fs::read_dir(nvm) {
            let mut candidates = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path().join("bin").join(program))
                .filter(|path| path.exists())
                .collect::<Vec<_>>();
            candidates.sort();
            if let Some(path) = candidates.pop() {
                return path;
            }
        }
    }
    PathBuf::from(program)
}

fn run(dir: Option<&Path>, program: &str, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new(program_path(program));
    command.args(args);
    if let Some(node_bin) = program_path("node").parent() {
        let mut paths = vec![node_bin.to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        if let Ok(path) = std::env::join_paths(paths) {
            command.env("PATH", path);
        }
    }
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = command
        .output()
        .map_err(|err| format!("无法运行 {program}: {err}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .chars()
            .take(800)
            .collect());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_with_timeout(
    dir: Option<&Path>,
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    let mut stdout_file = tempfile::tempfile()
        .map_err(|err| format!("无法创建 {program} 标准输出临时文件: {err}"))?;
    let mut stderr_file = tempfile::tempfile()
        .map_err(|err| format!("无法创建 {program} 标准错误临时文件: {err}"))?;
    let stdout_sink = stdout_file
        .try_clone()
        .map_err(|err| format!("无法准备 {program} 标准输出: {err}"))?;
    let stderr_sink = stderr_file
        .try_clone()
        .map_err(|err| format!("无法准备 {program} 标准错误: {err}"))?;
    let mut command = Command::new(program_path(program));
    command
        .args(args)
        .stdout(Stdio::from(stdout_sink))
        .stderr(Stdio::from(stderr_sink));
    #[cfg(unix)]
    command.process_group(0);
    if let Some(node_bin) = program_path("node").parent() {
        let mut paths = vec![node_bin.to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        if let Ok(path) = std::env::join_paths(paths) {
            command.env("PATH", path);
        }
    }
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let mut child = command
        .spawn()
        .map_err(|err| format!("无法运行 {program}: {err}"))?;
    let started = Instant::now();
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                terminate_process(&mut child);
                return Err(format!("运行 {program} 超时（超过 {:?}）", timeout));
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(err) => {
                terminate_process(&mut child);
                return Err(format!("等待 {program} 结束失败: {err}"));
            }
        }
    };
    stdout_file
        .seek(SeekFrom::Start(0))
        .map_err(|err| format!("无法读取 {program} 标准输出: {err}"))?;
    stderr_file
        .seek(SeekFrom::Start(0))
        .map_err(|err| format!("无法读取 {program} 标准错误: {err}"))?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    stdout_file
        .read_to_end(&mut stdout)
        .map_err(|err| format!("无法读取 {program} 标准输出: {err}"))?;
    stderr_file
        .read_to_end(&mut stderr)
        .map_err(|err| format!("无法读取 {program} 标准错误: {err}"))?;
    if !exit_status.success() {
        return Err(String::from_utf8_lossy(&stderr)
            .trim()
            .chars()
            .take(800)
            .collect());
    }
    Ok(String::from_utf8_lossy(&stdout).trim().to_string())
}

fn terminate_process(child: &mut Child) {
    #[cfg(unix)]
    if unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) } != 0 {
        let _ = child.kill();
    }
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        let _ = Command::new("taskkill")
            .args(["/PID", pid.as_str(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = child.kill();
    }
    #[cfg(not(any(unix, windows)))]
    let _ = child.kill();
    let _ = child.wait();
}

fn node_version() -> Option<String> {
    run(None, "node", &["--version"])
        .ok()
        .filter(|v| !v.is_empty())
}

fn node_supported(version: &str) -> bool {
    version
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0)
        >= 20
}

fn ensure_cloudflared() -> Result<(), String> {
    if run(None, "cloudflared", &["--version"]).is_ok() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        if run(None, "brew", &["--version"]).is_ok() {
            run_with_timeout(
                None,
                "brew",
                &["install", "cloudflared"],
                BRIDGE_SETUP_TIMEOUT,
            )?;
            return Ok(());
        }
    }
    Err("缺少 cloudflared；请先安装 cloudflared 后重试。".into())
}

fn browser_auth_supported(port: Option<u16>, running: bool) -> bool {
    if !running {
        return false;
    }
    let Some(port) = port else {
        return false;
    };
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2))
    else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    if write!(
        stream,
        "HEAD /headroom/connect HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .is_err()
    {
        return false;
    }
    BufReader::new(stream.take(8192))
        .lines()
        .take_while(|line| line.as_ref().is_ok_and(|line| !line.is_empty()))
        .filter_map(Result::ok)
        .any(|line| line.eq_ignore_ascii_case("x-headroom-browser-auth: 2"))
}

fn browser_authorization_url(
    endpoint: Option<&str>,
    code: Option<&str>,
    supported: bool,
) -> Option<String> {
    if !supported {
        return None;
    }
    let mut url = reqwest::Url::parse(endpoint?).ok()?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let code = code?;
    if code.is_empty()
        || !code
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        return None;
    }
    url.set_path("/headroom/connect");
    url.set_query(None);
    // The fragment stays in the browser and is removed before making an HTTP request.
    url.set_fragment(Some(&format!("pairing_code={code}")));
    Some(url.to_string())
}

fn ensure_browser_auth(source_path: &Path) -> Result<bool, String> {
    const PAYLOAD: &str = include_str!("../bridge/browser-auth.ts");
    const PATCH: &str = include_str!("../bridge/browser-auth.patch");
    let oauth_path = source_path.join("src/auth/oauth.ts");
    let helper_path = source_path.join("src/auth/browser-auth.ts");
    let oauth = fs::read_to_string(&oauth_path)
        .map_err(|error| format!("读取 Bridge 授权模块失败：{error}"))?;
    let current_helper = fs::read_to_string(&helper_path).ok();
    if current_helper
        .as_ref()
        .is_some_and(|text| !text.starts_with("// Headroom browser authorization"))
    {
        return Err("Bridge 中存在其他来源的 browser-auth.ts，请先检查文件冲突。".into());
    }
    let patched = oauth.contains("import { BrowserAuth } from \"./browser-auth.js\";");
    let changed = !patched
        || current_helper.as_deref() != Some(PAYLOAD)
        || !source_path.join("dist/auth/browser-auth.js").exists();
    if !patched {
        let mut patch = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
        patch
            .write_all(PATCH.as_bytes())
            .map_err(|error| error.to_string())?;
        let path = patch.path().to_string_lossy();
        run_with_timeout(
            Some(source_path),
            "git",
            &["apply", "--check", &path],
            BRIDGE_COMMAND_TIMEOUT,
        )
        .map_err(|_| "当前 Bridge 授权模块与内置适配不兼容，尚未修改授权逻辑。".to_string())?;
        run_with_timeout(
            Some(source_path),
            "git",
            &["apply", &path],
            BRIDGE_COMMAND_TIMEOUT,
        )?;
    }
    if current_helper.as_deref() != Some(PAYLOAD) {
        fs::write(&helper_path, PAYLOAD)
            .map_err(|error| format!("写入 Bridge 浏览器授权模块失败：{error}"))?;
    }
    Ok(changed)
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn normalize_workspace_path(path: PathBuf) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(&path) {
        return canonical;
    }

    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn resolve_workspace(explicit: Option<&str>) -> PathBuf {
    explicit
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(normalize_workspace_path)
        .or_else(|| {
            current_codex_workspace()
                .map(PathBuf::from)
                .map(resolve_auto_workspace)
        })
        .or_else(|| std::env::current_dir().ok().map(resolve_auto_workspace))
        .unwrap_or_else(|| resolve_auto_workspace(PathBuf::from(".")))
}

fn same_workspace(left: &str, right: &str) -> bool {
    normalize_workspace_path(PathBuf::from(left)) == normalize_workspace_path(PathBuf::from(right))
}

fn legacy_pairing_metadata_path(workspace: &str) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    workspace.hash(&mut hasher);
    root()
        .join("pairing")
        .join(format!("{:016x}.json", hasher.finish()))
}

fn pairing_metadata_path(workspace: &str) -> PathBuf {
    let digest = Sha256::digest(workspace.as_bytes());
    root().join("pairing").join(format!("{digest:x}.json"))
}

fn endpoint_metadata_path(workspace: &str) -> PathBuf {
    let digest = Sha256::digest(workspace.as_bytes());
    root()
        .join("pairing")
        .join(format!("{digest:x}.endpoint.json"))
}

fn observed_endpoint_metadata_path(workspace: &str) -> PathBuf {
    let digest = Sha256::digest(workspace.as_bytes());
    root()
        .join("pairing")
        .join(format!("{digest:x}.observed.endpoint.json"))
}

fn save_observed_endpoint_metadata(workspace: &str, value: &serde_json::Value) {
    let metadata = EndpointMetadata {
        public_url: value_string_aliases(Some(value), &["publicUrl", "public_url"]),
        mcp_url: value_string_aliases(Some(value), &["mcpUrl", "mcp_url"]),
        connector_name: value_string_aliases(
            Some(value),
            &["connectorName", "connector_name", "name"],
        ),
    };
    if metadata.public_url.is_none()
        && metadata.mcp_url.is_none()
        && metadata.connector_name.is_none()
    {
        return;
    }
    let path = observed_endpoint_metadata_path(workspace);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec(&metadata) {
        let _ = fs::write(path, bytes);
    }
}

fn normalize_endpoint(value: Option<&str>) -> Option<String> {
    let mut endpoint = value?.trim().trim_end_matches('/').to_string();
    if endpoint.to_ascii_lowercase().ends_with("/mcp") {
        endpoint.truncate(endpoint.len().saturating_sub(4));
        endpoint = endpoint.trim_end_matches('/').to_string();
    }
    (!endpoint.is_empty()).then_some(endpoint)
}

fn load_endpoint_metadata(workspace: &str) -> Option<EndpointMetadata> {
    let text = fs::read_to_string(endpoint_metadata_path(workspace)).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_endpoint_metadata(workspace: &str, value: &serde_json::Value) {
    let metadata = EndpointMetadata {
        public_url: value_string_aliases(Some(value), &["publicUrl", "public_url"]),
        mcp_url: value_string_aliases(Some(value), &["mcpUrl", "mcp_url"]),
        connector_name: value_string_aliases(
            Some(value),
            &["connectorName", "connector_name", "name"],
        ),
    };
    if metadata.public_url.is_none()
        && metadata.mcp_url.is_none()
        && metadata.connector_name.is_none()
    {
        return;
    }
    let path = endpoint_metadata_path(workspace);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec(&metadata) {
        if fs::write(&path, bytes).is_ok() {
            #[cfg(unix)]
            if let Ok(file_metadata) = fs::metadata(&path) {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = file_metadata.permissions();
                permissions.set_mode(0o600);
                let _ = fs::set_permissions(&path, permissions);
            }
        }
    }
}

fn endpoint_changed_safe(
    previous: Option<&EndpointMetadata>,
    public_url: Option<&str>,
    mcp_url: Option<&str>,
    connector_name: Option<&str>,
) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    let previous_endpoint = normalize_endpoint(previous.mcp_url.as_deref())
        .or_else(|| normalize_endpoint(previous.public_url.as_deref()));
    let current_endpoint = normalize_endpoint(mcp_url).or_else(|| normalize_endpoint(public_url));
    let Some(current_endpoint) = current_endpoint else {
        return false;
    };
    if let Some(previous_endpoint) = previous_endpoint {
        if previous_endpoint != current_endpoint {
            return true;
        }
    }
    match (
        previous
            .connector_name
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty()),
        connector_name.map(str::trim).filter(|v| !v.is_empty()),
    ) {
        (Some(previous), Some(current)) => previous != current,
        _ => false,
    }
}

fn save_pairing_metadata(workspace: &str, value: &serde_json::Value) {
    let Some(code) = pairing_code(Some(value)) else {
        return;
    };
    let Some(expires_at) = pairing_expires_at(Some(value)) else {
        return;
    };
    if expires_at <= now_millis() {
        return;
    }
    let metadata = PairingMetadata {
        pairing_code: code,
        pairing_expires_at: expires_at,
        mcp_url: value_string_aliases(Some(value), &["mcpUrl", "mcp_url"]),
        workspace_id: value_string(Some(value), "workspaceId"),
        workspace_name: value_string(Some(value), "workspaceName"),
    };
    let path = pairing_metadata_path(workspace);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec(&metadata) {
        if fs::write(&path, bytes).is_ok() {
            #[cfg(unix)]
            if let Ok(file_metadata) = fs::metadata(&path) {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = file_metadata.permissions();
                permissions.set_mode(0o600);
                let _ = fs::set_permissions(&path, permissions);
            }
        }
    }
}

fn load_pairing_metadata(workspace: &str) -> Option<PairingMetadata> {
    for path in [
        pairing_metadata_path(workspace),
        legacy_pairing_metadata_path(workspace),
    ] {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(metadata) = serde_json::from_str::<PairingMetadata>(&text) else {
            continue;
        };
        if metadata.pairing_expires_at <= now_millis() {
            let _ = fs::remove_file(path);
            continue;
        }
        return Some(metadata);
    }
    None
}

fn clear_pairing_metadata(workspace: &str) {
    let _ = fs::remove_file(pairing_metadata_path(workspace));
    let _ = fs::remove_file(legacy_pairing_metadata_path(workspace));
}

fn runtime_json(workspace: Option<&str>) -> Option<serde_json::Value> {
    let dir = root().join("runtime");
    let entries = fs::read_dir(dir).ok()?;
    let mut values = Vec::<(SystemTime, serde_json::Value)>::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
            continue;
        };
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if let Some(workspace) = workspace {
            let Some(runtime_workspace) = value_string(Some(&value), "workspaceRoot") else {
                continue;
            };
            if !same_workspace(&runtime_workspace, workspace) {
                continue;
            }
        }
        values.push((modified, value));
    }
    values
        .into_iter()
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, value)| value)
}

fn session_cwd(path: &Path) -> Option<PathBuf> {
    let file = fs::File::open(path).ok()?;
    for line in BufReader::new(file).lines().take(200).flatten() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(serde_json::Value::as_str) != Some("session_meta") {
            continue;
        }
        let cwd = value
            .get("payload")
            .and_then(|payload| payload.get("cwd"))
            .and_then(serde_json::Value::as_str)
            .filter(|cwd| !cwd.is_empty())?;
        let path = PathBuf::from(cwd);
        return path.is_dir().then_some(path);
    }
    None
}

fn session_file_for_id(dir: &Path, session_id: &str, newest: &mut Option<(SystemTime, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            session_file_for_id(&path, session_id, newest);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl")
            || !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(session_id))
        {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
            continue;
        };
        if newest
            .as_ref()
            .map(|(current, _)| modified > *current)
            .unwrap_or(true)
        {
            *newest = Some((modified, path));
        }
    }
}

fn newest_session_file(dir: &Path, newest: &mut Option<(std::time::SystemTime, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            newest_session_file(&path, newest);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
                continue;
            };
            if newest
                .as_ref()
                .map(|(current, _)| modified > *current)
                .unwrap_or(true)
            {
                *newest = Some((modified, path));
            }
        }
    }
}

fn git_workspace_root(path: &Path) -> Option<PathBuf> {
    let mut current = normalize_workspace_path(path.to_path_buf());
    if !current.is_dir() {
        current = current.parent()?.to_path_buf();
    }
    loop {
        if current.join(".git").exists() {
            return Some(normalize_workspace_path(current));
        }
        if !current.pop() {
            break;
        }
    }
    None
}

fn collect_nested_git_roots(path: &Path, depth: usize, roots: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let candidate = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir()
            || candidate.file_name().and_then(|name| name.to_str()) == Some(".git")
        {
            continue;
        }
        if candidate.join(".git").exists() {
            roots.push(normalize_workspace_path(candidate));
            continue;
        }
        if depth > 0 {
            collect_nested_git_roots(&candidate, depth - 1, roots);
        }
    }
}

fn unique_nested_git_root(path: &Path) -> Option<PathBuf> {
    let root = normalize_workspace_path(path.to_path_buf());
    if !root.is_dir() {
        return None;
    }
    let mut roots = Vec::new();
    collect_nested_git_roots(&root, 2, &mut roots);
    roots.sort();
    roots.dedup();
    (roots.len() == 1).then(|| roots.remove(0))
}

fn resolve_auto_workspace(path: PathBuf) -> PathBuf {
    if let Some(root) = git_workspace_root(&path) {
        return root;
    }
    unique_nested_git_root(&path).unwrap_or_else(|| normalize_workspace_path(path))
}

fn current_codex_workspace() -> Option<String> {
    let sessions = dirs::home_dir()?.join(".codex/sessions");
    let mut newest = None;
    for variable in ["CODEX_SESSION_ID", "CODEX_THREAD_ID"] {
        let Ok(session_id) = std::env::var(variable) else {
            continue;
        };
        let session_id = session_id.trim();
        if session_id.is_empty() {
            continue;
        }
        session_file_for_id(&sessions, session_id, &mut newest);
        if newest.is_some() {
            break;
        }
    }
    if newest.is_none() {
        newest_session_file(&sessions, &mut newest);
    }
    session_cwd(&newest?.1).map(|path| path.display().to_string())
}

fn value_string(value: Option<&serde_json::Value>, key: &str) -> Option<String> {
    value
        .and_then(|value| value.get(key))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
}

fn value_u64(value: Option<&serde_json::Value>, key: &str) -> Option<u64> {
    value
        .and_then(|value| value.get(key))
        .and_then(serde_json::Value::as_u64)
}

fn value_bool(value: Option<&serde_json::Value>, key: &str) -> Option<bool> {
    value
        .and_then(|value| value.get(key))
        .and_then(serde_json::Value::as_bool)
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    if key == "tokencount" {
        return false;
    }
    [
        "token",
        "secret",
        "password",
        "apikey",
        "api_key",
        "privatekey",
        "private_key",
        "admintoken",
        "pairingcode",
        "pairing_code",
        "pairingexpiresat",
        "expiresat",
    ]
    .iter()
    .any(|part| key.contains(part))
}

fn sanitized_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let value = if is_sensitive_key(key) {
                        serde_json::Value::String("[已隐藏]".into())
                    } else {
                        sanitized_json(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(sanitized_json).collect())
        }
        _ => value.clone(),
    }
}

fn safe_output(output: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(output) {
        return serde_json::to_string_pretty(&sanitized_json(&value))
            .unwrap_or_else(|_| "[输出无法显示]".into());
    }
    output
        .lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if [
                "token",
                "secret",
                "password",
                "apikey",
                "api_key",
                "private_key",
                "admin_token",
                "pairing_code",
                "pairingcode",
                "pairing_expires_at",
                "pairingexpiresat",
            ]
            .iter()
            .any(|key| lower.contains(key))
            {
                "[已隐藏敏感输出]"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn pairing_code(value: Option<&serde_json::Value>) -> Option<String> {
    ["pairingCode", "pairing_code"]
        .iter()
        .find_map(|key| value_string(value, key))
}

fn value_string_aliases(value: Option<&serde_json::Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| value_string(value, key))
}

fn pairing_expires_at(value: Option<&serde_json::Value>) -> Option<u64> {
    [
        "pairingExpiresAt",
        "pairing_expires_at",
        "expiresAt",
        "expires_at",
    ]
    .iter()
    .find_map(|key| {
        value.and_then(|value| value.get(key)).and_then(|raw| {
            let timestamp = raw
                .as_u64()
                .or_else(|| raw.as_str().and_then(|text| text.parse::<u64>().ok()))?;
            Some(if timestamp < 100_000_000_000 {
                timestamp.saturating_mul(1_000)
            } else {
                timestamp
            })
        })
    })
}

fn paired_from_cli(value: Option<&serde_json::Value>) -> bool {
    value
        .and_then(|value| value_bool(Some(value), "paired"))
        .or_else(|| value.and_then(|value| value_bool(Some(value), "authorized")))
        .or_else(|| {
            value
                .and_then(|value| value_u64(Some(value), "tokenCount"))
                .map(|count| count > 0)
        })
        .unwrap_or(false)
}

/// A valid pairing code is the source of truth while OAuth is waiting.
/// The CLI may temporarily report `pairingActive: false` between commands;
/// allowing that value to win makes the App hide a still-valid code.
fn pairing_active_for(
    paired: bool,
    pairing_code: Option<&str>,
    pairing_expires_at: Option<u64>,
    reported_active: Option<bool>,
    now: u64,
) -> Option<bool> {
    if paired {
        return Some(false);
    }
    if pairing_code.is_some() {
        return Some(
            pairing_expires_at
                .map(|expires| expires > now)
                .unwrap_or(true),
        );
    }
    reported_active
}

fn ensure_stopped(result: Result<CodexBridgeStatus, String>) -> Result<(), String> {
    result.map(|_| ())
}

fn parse_json_output(output: &str) -> Option<serde_json::Value> {
    serde_json::from_str(output).ok().or_else(|| {
        output
            .lines()
            .rev()
            .find_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
    })
}

fn merge_status_values(
    primary: Option<&serde_json::Value>,
    fallback: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    match (primary, fallback) {
        (Some(primary), Some(fallback)) => {
            let Some(primary_object) = primary.as_object() else {
                return Some(primary.clone());
            };
            let mut merged = fallback.as_object().cloned().unwrap_or_default();
            for (key, value) in primary_object {
                if !value.is_null() {
                    merged.insert(key.clone(), value.clone());
                }
            }
            Some(serde_json::Value::Object(merged))
        }
        (Some(value), None) | (None, Some(value)) => Some(value.clone()),
        (None, None) => None,
    }
}

pub fn status(workspace: Option<&str>) -> CodexBridgeStatus {
    let source_path = source();
    let node = node_version();
    let resolved_workspace = resolve_workspace(workspace);
    let workspace_string = resolved_workspace.display().to_string();
    let runtime = runtime_json(Some(&workspace_string));
    let cli_status = if source_path.join("dist/cli/index.js").exists() {
        run_with_timeout(
            Some(&source_path),
            "node",
            &[
                "bin/c2c.js",
                "status",
                "--workspace",
                &workspace_string,
                "--json",
            ],
            BRIDGE_COMMAND_TIMEOUT,
        )
        .ok()
        .and_then(|text| parse_json_output(&text))
    } else {
        None
    };
    let tunnel_status = if source_path.join("dist/cli/index.js").exists() {
        run_with_timeout(
            Some(&source_path),
            "node",
            &[
                "bin/c2c.js",
                "tunnel",
                "status",
                "--workspace",
                &workspace_string,
                "--json",
            ],
            BRIDGE_COMMAND_TIMEOUT,
        )
        .ok()
        .and_then(|text| parse_json_output(&text))
    } else {
        None
    };
    let metadata = load_pairing_metadata(&workspace_string);
    let merged_status = merge_status_values(cli_status.as_ref(), runtime.as_ref());
    let status_value = merged_status.as_ref();
    // A runtime record is only a fallback when the CLI status is unavailable.
    // This avoids reporting a stale runtime file as an active process.
    let running = cli_status
        .as_ref()
        .and_then(|value| value_bool(Some(value), "running"))
        .or_else(|| {
            if cli_status.is_none() {
                runtime
                    .as_ref()
                    .and_then(|value| value_bool(Some(value), "running"))
            } else {
                None
            }
        })
        .unwrap_or(false);
    let token_count = value_u64(status_value, "tokenCount");
    // A stale runtime file is useful for display metadata, but it must never
    // be treated as proof of a live authorization when the CLI status query
    // failed. Authorization comes from the live CLI response first.
    let paired = paired_from_cli(cli_status.as_ref());
    let pairing_code = if paired {
        None
    } else {
        pairing_code(status_value)
            .or_else(|| metadata.as_ref().map(|value| value.pairing_code.clone()))
    };
    let pairing_expires_at = if paired {
        None
    } else {
        pairing_expires_at(status_value)
            .or_else(|| metadata.as_ref().map(|value| value.pairing_expires_at))
    };
    let pairing_active = pairing_active_for(
        paired,
        pairing_code.as_deref(),
        pairing_expires_at,
        value_bool(status_value, "pairingActive"),
        now_millis(),
    );
    if paired {
        clear_pairing_metadata(&workspace_string);
    }
    let workspace_name = value_string(status_value, "workspaceName").or_else(|| {
        metadata
            .as_ref()
            .and_then(|value| value.workspace_name.clone())
    });
    let workspace_id = value_string(status_value, "workspaceId").or_else(|| {
        metadata
            .as_ref()
            .and_then(|value| value.workspace_id.clone())
    });
    let public_url = value_string_aliases(status_value, &["publicUrl", "public_url"]);
    let tunnel_mode = tunnel_status.as_ref().and_then(|value| {
        if value_bool(Some(value), "namedReady") == Some(true) {
            Some("named".to_string())
        } else {
            value_string(Some(value), "preference")
        }
    });
    let tunnel_hostname = tunnel_status
        .as_ref()
        .and_then(|value| value_string(Some(value), "hostname"));
    let tunnel_fallback_reason = tunnel_status
        .as_ref()
        .and_then(|value| value_string(Some(value), "fallbackReason"));
    let tunnel_stable = tunnel_status
        .as_ref()
        .and_then(|value| value_bool(Some(value), "namedReady"))
        == Some(true);
    let mcp_url = value_string_aliases(
        status_value,
        &["mcpUrl", "mcp_url", "connectorUrl", "connector_url"],
    )
    .or_else(|| metadata.as_ref().and_then(|value| value.mcp_url.clone()));
    let connector_name =
        value_string_aliases(status_value, &["connectorName", "connector_name", "name"]).or_else(
            || load_endpoint_metadata(&workspace_string).and_then(|value| value.connector_name),
        );
    let previous_endpoint = load_endpoint_metadata(&workspace_string);
    if previous_endpoint.is_none() && (public_url.is_some() || mcp_url.is_some()) {
        save_endpoint_metadata(
            &workspace_string,
            &serde_json::json!({
                "publicUrl": public_url.clone(),
                "mcpUrl": mcp_url.clone(),
                "connectorName": connector_name.clone(),
            }),
        );
    }
    let endpoint_changed = endpoint_changed_safe(
        previous_endpoint.as_ref(),
        public_url.as_deref(),
        mcp_url.as_deref(),
        connector_name.as_deref(),
    );
    let endpoint_repair_required = endpoint_changed;
    let port = value_u64(status_value, "port").and_then(|value| u16::try_from(value).ok());
    let browser_auth_supported = browser_auth_supported(port, running);
    let authorization_url = browser_authorization_url(
        mcp_url.as_deref().or(public_url.as_deref()),
        pairing_code.as_deref(),
        browser_auth_supported,
    );
    let installed = source_path.join("dist/cli/index.js").exists();
    let message = if !installed {
        "尚未安装".into()
    } else if running {
        "Bridge 正在运行".into()
    } else {
        "已安装，尚未启动".into()
    };
    CodexBridgeStatus {
        installed,
        node_available: node.is_some(),
        node_version: node,
        source_path: source_path.display().to_string(),
        browser_auth_supported,
        running,
        port,
        paired,
        codex_logged_in: crate::client_adapters::codex_logged_in(),
        message,
        output: None,
        workspace_name,
        workspace_id,
        public_url,
        tunnel_mode,
        tunnel_hostname,
        tunnel_stable,
        tunnel_fallback_reason,
        token_count,
        pairing_active,
        pairing_code,
        pairing_expires_at,
        authorization_url,
        mcp_url,
        connector_name,
        endpoint_changed,
        endpoint_repair_required,
        safe_output: None,
        workspace_path: Some(workspace_string),
    }
}

pub async fn install(workspace: Option<String>) -> Result<CodexBridgeStatus, String> {
    let node = node_version().ok_or_else(|| "需要先安装 Node.js 20+。".to_string())?;
    if !node_supported(&node) {
        return Err(format!("当前 Node.js 为 {node}，需要 Node.js 20+。"));
    }
    std::fs::create_dir_all(root()).map_err(|e| format!("创建 Bridge 目录失败: {e}"))?;
    let source_path = source();
    if source_path.join(".git").exists() {
        run_with_timeout(
            Some(&source_path),
            "git",
            &["pull", "--ff-only"],
            BRIDGE_SETUP_TIMEOUT,
        )?;
    } else {
        if source_path.exists() {
            std::fs::remove_dir_all(&source_path)
                .map_err(|e| format!("清理旧 Bridge 目录失败: {e}"))?;
        }
        run_with_timeout(
            Some(&root()),
            "git",
            &["clone", "--depth", "1", REPOSITORY, "source"],
            BRIDGE_SETUP_TIMEOUT,
        )?;
    }
    run_with_timeout(
        Some(&source_path),
        "npx",
        &["--yes", PACKAGE_MANAGER, "install", "--frozen-lockfile"],
        BRIDGE_SETUP_TIMEOUT,
    )?;
    ensure_browser_auth(&source_path)?;
    run_with_timeout(
        Some(&source_path),
        "npx",
        &["--yes", PACKAGE_MANAGER, "build"],
        BRIDGE_SETUP_TIMEOUT,
    )?;
    ensure_cloudflared()?;
    Ok(status(workspace.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::{
        endpoint_changed_safe, ensure_stopped, merge_status_values, node_supported,
        normalize_endpoint, paired_from_cli, pairing_active_for, pairing_expires_at,
        parse_json_output, remove_bridge_dir_after_stop, resolve_auto_workspace, run_with_timeout,
        safe_output, same_workspace, EndpointMetadata,
    };
    use serde_json::json;

    #[test]
    fn requires_node_twenty_or_newer() {
        assert!(!node_supported("v18.20.0"));
        assert!(node_supported("v20.0.0"));
        assert!(node_supported("v26.3.0"));
    }

    #[test]
    fn browser_verification_credential_stays_in_fragment_and_requires_https() {
        let url = super::browser_authorization_url(
            Some("https://bridge.example/mcp?old=1#old"),
            Some("ABCD-EFGH"),
            true,
        )
        .unwrap();
        let parsed = reqwest::Url::parse(&url).unwrap();
        assert_eq!(parsed.path(), "/headroom/connect");
        assert_eq!(parsed.query(), None);
        assert_eq!(parsed.fragment(), Some("pairing_code=ABCD-EFGH"));
        assert!(super::browser_authorization_url(
            Some("http://bridge.example/mcp"),
            Some("ABCD-EFGH"),
            true
        )
        .is_none());
        assert!(super::browser_authorization_url(
            Some("https://bridge.example/mcp"),
            Some("ABCD-EFGH&other=1"),
            true
        )
        .is_none());
        assert!(super::browser_authorization_url(
            Some("https://bridge.example/mcp"),
            Some("ABCD-EFGH"),
            false
        )
        .is_none());
        assert!(
            super::browser_authorization_url(Some("https://bridge.example/mcp"), None, true)
                .is_none()
        );
    }

    #[test]
    fn safe_output_redacts_credentials_but_keeps_status_summary() {
        let output = r#"{"workspaceName":"Demo","tokenCount":2,"adminToken":"do-not-show","pairingCode":"ABCD2345","pairingActive":true}"#;
        let safe = safe_output(output);
        assert!(safe.contains("Demo"));
        assert!(safe.contains("tokenCount"));
        assert!(safe.contains("[已隐藏]"));
        assert!(!safe.contains("do-not-show"));
        assert!(!safe.contains("ABCD2345"));
    }

    #[test]
    fn safe_output_redacts_sensitive_plain_text_lines() {
        let safe = safe_output("workspace ready\nadminToken=do-not-show\nport=48765");
        assert!(safe.contains("workspace ready"));
        assert!(safe.contains("port=48765"));
        assert!(!safe.contains("do-not-show"));
    }

    #[test]
    fn parses_json_after_cli_log_lines() {
        let parsed = parse_json_output("starting bridge\n{\"pairingCode\":\"ABCD2345\"}")
            .expect("last JSON line should be parsed");
        assert_eq!(parsed["pairingCode"], "ABCD2345");
    }

    #[test]
    fn accepts_expiry_aliases_and_second_timestamps() {
        let value = json!({"expiresAt": "1700000000"});
        assert_eq!(pairing_expires_at(Some(&value)), Some(1_700_000_000_000));
    }

    #[test]
    fn valid_pairing_code_overrides_transient_inactive_report() {
        assert_eq!(
            pairing_active_for(
                false,
                Some("ABCD2345"),
                Some(2_000_000),
                Some(false),
                1_000_000,
            ),
            Some(true)
        );
    }

    #[test]
    fn pairing_code_is_inactive_after_expiry_but_pairing_wins() {
        assert_eq!(
            pairing_active_for(
                false,
                Some("ABCD2345"),
                Some(1_000_000),
                Some(true),
                1_000_001
            ),
            Some(false)
        );
        assert_eq!(
            pairing_active_for(
                true,
                Some("ABCD2345"),
                Some(2_000_000),
                Some(true),
                1_000_000
            ),
            Some(false)
        );
    }

    #[test]
    fn merges_missing_cli_fields_from_runtime() {
        let cli = json!({"running": false, "tokenCount": null});
        let runtime = json!({"tokenCount": 3, "workspaceName": "Demo"});
        let merged = merge_status_values(Some(&cli), Some(&runtime)).expect("merged status");
        assert_eq!(merged["tokenCount"], 3);
        assert_eq!(merged["workspaceName"], "Demo");
        assert_eq!(merged["running"], false);
    }

    #[test]
    fn missing_live_authorization_fields_do_not_use_runtime_token_count() {
        let cli = json!({});
        assert!(!paired_from_cli(Some(&cli)));
    }

    #[test]
    fn stop_failure_preserves_bridge_data_before_uninstall() {
        let root = tempfile::tempdir().expect("tempdir");
        let bridge_dir = root.path().join("bridge");
        std::fs::create_dir_all(&bridge_dir).expect("bridge dir");
        let marker = bridge_dir.join("keep-me");
        std::fs::write(&marker, b"bridge data").expect("bridge marker");

        let result = remove_bridge_dir_after_stop(&bridge_dir, Err("stop failed".into()));
        assert_eq!(result, Err("stop failed".into()));
        assert!(marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn timed_out_bridge_command_is_killed_and_reaped() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("descendant-finished");
        let marker_arg = marker.to_string_lossy().into_owned();
        let started = std::time::Instant::now();
        let result = run_with_timeout(
            None,
            "sh",
            &[
                "-c",
                "(sleep 1; touch \"$1\") & wait",
                "bridge-timeout",
                marker_arg.as_str(),
            ],
            std::time::Duration::from_millis(50),
        );
        assert!(result
            .expect_err("command should time out")
            .contains("超时"));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        std::thread::sleep(std::time::Duration::from_millis(1200));
        assert!(!marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn timed_bridge_command_captures_output_without_pipes() {
        let output = run_with_timeout(
            None,
            "sh",
            &["-c", "printf bridge-output"],
            std::time::Duration::from_secs(1),
        )
        .expect("command output");
        assert_eq!(output, "bridge-output");
    }

    #[test]
    fn compares_equivalent_workspace_paths() {
        assert!(same_workspace(
            "/tmp/headroom/../headroom/project/",
            "/tmp/headroom/project"
        ));
    }

    #[test]
    fn resolves_a_unique_nested_git_workspace() {
        let outer = tempfile::tempdir().expect("outer tempdir");
        let nested = outer.path().join("Headroom-desktop-CN");
        std::fs::create_dir_all(nested.join(".git")).expect("nested git marker");

        assert_eq!(
            resolve_auto_workspace(outer.path().to_path_buf()),
            std::fs::canonicalize(nested).expect("canonical nested workspace")
        );
    }

    #[test]
    fn keeps_an_ambiguous_parent_workspace_instead_of_guessing() {
        let outer = tempfile::tempdir().expect("outer tempdir");
        for name in ["first", "second"] {
            std::fs::create_dir_all(outer.path().join(name).join(".git")).expect("git marker");
        }

        assert_eq!(
            resolve_auto_workspace(outer.path().to_path_buf()),
            std::fs::canonicalize(outer.path()).expect("canonical outer workspace")
        );
    }

    #[test]
    fn promotes_a_path_inside_a_git_workspace_to_its_root() {
        let outer = tempfile::tempdir().expect("outer tempdir");
        let nested = outer.path().join("repo");
        let child = nested.join("src");
        std::fs::create_dir_all(nested.join(".git")).expect("git marker");
        std::fs::create_dir_all(&child).expect("child directory");

        assert_eq!(
            resolve_auto_workspace(child),
            std::fs::canonicalize(nested).expect("canonical workspace root")
        );
    }

    #[test]
    fn normalizes_mcp_suffix_and_trailing_slash() {
        assert_eq!(
            normalize_endpoint(Some("https://example.test/mcp/")),
            normalize_endpoint(Some("https://example.test"))
        );
    }

    #[test]
    fn detects_endpoint_change() {
        let previous = EndpointMetadata {
            public_url: Some("https://old.example.test/".into()),
            mcp_url: Some("https://old.example.test/mcp".into()),
            connector_name: Some("Codex".into()),
        };
        assert!(endpoint_changed_safe(
            Some(&previous),
            Some("https://new.example.test"),
            Some("https://new.example.test/mcp/"),
            Some("Codex")
        ));
        assert!(!endpoint_changed_safe(
            Some(&previous),
            Some("https://old.example.test/mcp"),
            Some("https://old.example.test/mcp/"),
            Some("Codex")
        ));
    }

    #[test]
    fn endpoint_change_survives_restart_until_confirmed_endpoint_is_acknowledged() {
        let confirmed = EndpointMetadata {
            public_url: Some("https://old.example.test".into()),
            mcp_url: Some("https://old.example.test/mcp".into()),
            connector_name: Some("Codex".into()),
        };
        // A newly observed endpoint must not replace the confirmed record.
        assert!(endpoint_changed_safe(
            Some(&confirmed),
            Some("https://new.example.test"),
            Some("https://new.example.test/mcp"),
            Some("Codex")
        ));
        // After ack_endpoint persists the new endpoint, a fresh status is clean.
        let acknowledged = EndpointMetadata {
            public_url: Some("https://new.example.test".into()),
            mcp_url: Some("https://new.example.test/mcp".into()),
            connector_name: Some("Codex".into()),
        };
        assert!(!endpoint_changed_safe(
            Some(&acknowledged),
            Some("https://new.example.test"),
            Some("https://new.example.test/mcp"),
            Some("Codex")
        ));
    }

    #[test]
    fn endpoint_comparison_is_workspace_scoped_by_metadata() {
        let previous = EndpointMetadata {
            public_url: Some("https://one.example.test".into()),
            mcp_url: None,
            connector_name: None,
        };
        assert!(!endpoint_changed_safe(
            Some(&previous),
            Some("https://one.example.test"),
            None,
            None
        ));
        assert!(endpoint_changed_safe(
            Some(&previous),
            Some("https://two.example.test"),
            None,
            None
        ));
    }

    #[test]
    fn unavailable_endpoint_does_not_require_repair() {
        let previous = EndpointMetadata {
            public_url: Some("https://one.example.test".into()),
            mcp_url: Some("https://one.example.test/mcp".into()),
            connector_name: Some("Codex".into()),
        };
        assert!(!endpoint_changed_safe(Some(&previous), None, None, None));
        assert!(!endpoint_changed_safe(
            Some(&previous),
            Some("https://one.example.test"),
            None,
            None
        ));
    }
}

pub async fn action(
    action: String,
    workspace: Option<String>,
) -> Result<CodexBridgeStatus, String> {
    let source_path = source();
    if !source_path.join("dist/cli/index.js").exists() {
        return Err("请先安装 Codex with ChatGPT。".into());
    }
    let command = match action.as_str() {
        "setup" | "doctor" | "status" | "pair" | "unpair" | "logs" | "stop" | "ack_endpoint" => {
            action
        }
        _ => return Err("不支持的 Bridge 操作。".into()),
    };
    let workspace_path = resolve_workspace(workspace.as_deref());
    let workspace = workspace_path.display().to_string();
    if matches!(command.as_str(), "setup" | "pair") {
        if ensure_browser_auth(&source_path)? {
            run_with_timeout(
                Some(&source_path),
                "npx",
                &["--yes", PACKAGE_MANAGER, "build"],
                BRIDGE_SETUP_TIMEOUT,
            )?;
        }
        let live = status(Some(&workspace));
        // Upgrade only the selected workspace's old server; other workspace processes keep running.
        if live.running && !live.browser_auth_supported {
            run_with_timeout(
                Some(&source_path),
                "node",
                &["bin/c2c.js", "stop", "--workspace", &workspace],
                BRIDGE_COMMAND_TIMEOUT,
            )?;
        }
    }
    if command == "ack_endpoint" {
        let mut status = status(Some(&workspace));
        let value = serde_json::json!({
            "publicUrl": status.public_url.clone(),
            "mcpUrl": status.mcp_url.clone(),
            "connectorName": status.connector_name.clone(),
        });
        save_endpoint_metadata(&workspace, &value);
        status.endpoint_changed = false;
        status.endpoint_repair_required = false;
        return Ok(status);
    }
    let mut args = vec![
        "bin/c2c.js",
        command.as_str(),
        "--workspace",
        workspace.as_str(),
    ];
    if matches!(command.as_str(), "setup" | "status" | "doctor" | "pair") {
        args.push("--json");
    }
    let timeout = if matches!(command.as_str(), "setup" | "doctor" | "pair") {
        BRIDGE_SETUP_TIMEOUT
    } else {
        BRIDGE_COMMAND_TIMEOUT
    };
    let output = run_with_timeout(Some(&source_path), "node", &args, timeout)?;
    let parsed = parse_json_output(&output);
    let previous_endpoint = load_endpoint_metadata(&workspace);
    if matches!(command.as_str(), "setup" | "pair") {
        if let Some(value) = parsed.as_ref() {
            save_pairing_metadata(&workspace, value);
        }
    }
    if command == "unpair" {
        clear_pairing_metadata(&workspace);
    }
    let mut status = status(Some(&workspace));
    if let Some(value) = parsed.as_ref() {
        status.workspace_name =
            value_string(Some(value), "workspaceName").or(status.workspace_name);
        status.workspace_id = value_string(Some(value), "workspaceId").or(status.workspace_id);
        status.mcp_url =
            value_string_aliases(Some(value), &["mcpUrl", "mcp_url"]).or(status.mcp_url);
        status.connector_name =
            value_string_aliases(Some(value), &["connectorName", "connector_name", "name"])
                .or(status.connector_name);
        status.authorization_url = value_string_aliases(
            Some(value),
            &[
                "authorizationUrl",
                "authorization_url",
                "authUrl",
                "auth_url",
            ],
        )
        .or(status.authorization_url);
        status.pairing_active = value_bool(Some(value), "pairingActive").or(status.pairing_active);
        if status.paired {
            // The status query may observe authorization completing while the
            // setup command is still returning. Never resurrect a consumed code.
            status.pairing_code = None;
            status.pairing_expires_at = None;
            status.pairing_active = Some(false);
        } else if let Some(expires_at) = pairing_expires_at(Some(value)) {
            if expires_at > now_millis() {
                status.pairing_expires_at = Some(expires_at);
                status.pairing_code = pairing_code(Some(value)).or(status.pairing_code);
            }
        }
        status.pairing_active = pairing_active_for(
            status.paired,
            status.pairing_code.as_deref(),
            status.pairing_expires_at,
            value_bool(Some(value), "pairingActive"),
            now_millis(),
        );
    }
    if let Some(value) = parsed.as_ref() {
        let next_public_url = value_string_aliases(Some(value), &["publicUrl", "public_url"])
            .or_else(|| status.public_url.clone());
        let next_mcp_url = value_string_aliases(Some(value), &["mcpUrl", "mcp_url"])
            .or_else(|| status.mcp_url.clone());
        status.endpoint_changed = endpoint_changed_safe(
            previous_endpoint.as_ref(),
            next_public_url.as_deref(),
            next_mcp_url.as_deref(),
            status.connector_name.as_deref(),
        );
        status.endpoint_repair_required = status.endpoint_changed;
        if matches!(command.as_str(), "setup" | "pair") {
            if previous_endpoint.is_some() {
                save_observed_endpoint_metadata(&workspace, value);
            } else {
                save_endpoint_metadata(&workspace, value);
            }
        }
    }
    let safe = (!output.is_empty()).then(|| safe_output(&output));
    status.authorization_url = browser_authorization_url(
        status.mcp_url.as_deref().or(status.public_url.as_deref()),
        status.pairing_code.as_deref(),
        status.browser_auth_supported,
    );
    status.output = safe.clone();
    status.safe_output = safe;
    Ok(status)
}

/// Configure the public endpoint without exposing the external Bridge CLI to the UI.
/// A named tunnel is persisted by c2c and reused by later setup/restarts.
pub async fn configure_tunnel(
    mode: String,
    zone: Option<String>,
    workspace: Option<String>,
) -> Result<CodexBridgeStatus, String> {
    let source_path = source();
    if !source_path.join("dist/cli/index.js").exists() {
        return Err("请先安装 Codex with ChatGPT。".into());
    }
    let workspace_path = resolve_workspace(workspace.as_deref());
    let workspace = workspace_path.display().to_string();
    let mode = mode.trim().to_ascii_lowercase();
    if mode != "named" && mode != "quick" {
        return Err("隧道模式必须是 named 或 quick。".into());
    }
    let mut args = vec![
        "bin/c2c.js",
        "tunnel",
        "choose",
        "--mode",
        mode.as_str(),
        "--workspace",
        workspace.as_str(),
        "--json",
    ];
    let zone_value = zone.unwrap_or_default();
    if mode == "named" && !zone_value.trim().is_empty() {
        args.extend(["--zone", zone_value.trim()]);
    }
    let output = run_with_timeout(Some(&source_path), "node", &args, BRIDGE_SETUP_TIMEOUT)?;
    let parsed = parse_json_output(&output).unwrap_or_else(|| {
        serde_json::json!({
            "ok": false,
            "userMessage": output,
        })
    });
    if parsed.get("ok").and_then(serde_json::Value::as_bool) == Some(false) && mode == "named" {
        let message = parsed
            .get("userMessage")
            .and_then(serde_json::Value::as_str)
            .or_else(|| parsed.get("error").and_then(serde_json::Value::as_str))
            .unwrap_or("固定域名配置未完成");
        return Err(message.to_string());
    }
    if mode == "named"
        && parsed
            .get("namedReady")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
    {
        return Err("固定域名尚未就绪，请完成 Cloudflare 登录和域名选择后重试。".into());
    }
    if mode == "named" {
        run_with_timeout(
            Some(&source_path),
            "node",
            &[
                "bin/c2c.js",
                "start",
                "--workspace",
                workspace.as_str(),
                "--tunnel",
                "--json",
            ],
            BRIDGE_SETUP_TIMEOUT,
        )?;
    }
    Ok(status(Some(&workspace)))
}

pub async fn uninstall(workspace: Option<String>) -> Result<CodexBridgeStatus, String> {
    let stop_result = if source().join("dist/cli/index.js").exists() {
        ensure_stopped(action("stop".into(), workspace).await)
    } else {
        Ok(())
    };
    let dir = root();
    remove_bridge_dir_after_stop(&dir, stop_result)?;
    Ok(status(None))
}

fn remove_bridge_dir_after_stop(dir: &Path, stop_result: Result<(), String>) -> Result<(), String> {
    stop_result?;
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|e| format!("移除 Bridge 数据失败: {e}"))?;
    }
    Ok(())
}
