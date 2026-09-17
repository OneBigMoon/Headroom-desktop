//! Stable local router for Codex.
//!
//! Codex keeps the provider URL in memory for the lifetime of a session.  A
//! desktop process which writes `config.toml` back to the native provider when
//! it exits therefore cannot make an already running Codex session follow the
//! change.  This small, independent listener gives Codex one URL that never
//! changes (`127.0.0.1:6891`): while Headroom is alive it relays bytes to the
//! normal intercept on `6867`; once the Headroom heartbeat becomes stale it
//! forwards the request directly to the native OpenAI/ChatGPT endpoint.
//!
//! The listener is started as a detached copy of the desktop executable.  It
//! enters this module before Tauri is initialised, so a GUI crash does not take
//! the listener down with it.  The GUI only owns a short heartbeat file and a
//! mode bit.  A missing, malformed, or stale file always selects the direct
//! path (fail-open for the user's Codex account).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream as StdTcpStream};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use base64::Engine;
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The stable URL written into Codex's provider block.
pub const CODEX_ROUTER_PORT: u16 = 6891;
/// OpenAI API-key mode uses the public-compatible `/v1` route.
pub const CODEX_ROUTER_OPENAI_BASE_URL: &str = "http://127.0.0.1:6891/v1";
/// ChatGPT OAuth mode must retain Codex's native backend path.  Codex derives
/// several account/search/realtime endpoints from this suffix, so pointing an
/// OAuth session at the generic `/v1` base loses those routes.
pub const CODEX_ROUTER_CHATGPT_BASE_URL: &str = "http://127.0.0.1:6891/backend-api/codex";
/// The existing Headroom in-process intercept.  This is deliberately kept
/// separate from the router port; the Python backend may use 6868..=6890.
pub const INTERCEPT_PORT: u16 = 6867;

/// A loopback-only, side-effect-free response emitted by Headroom's
/// in-process intercept. The detached router probes this endpoint before
/// relaying a Codex request so an unrelated process that happens to occupy
/// :6867 cannot receive credentials or request bodies.
pub const INTERCEPT_IDENTITY_PATH: &str = "/__headroom_intercept_identity";
pub const INTERCEPT_IDENTITY_HEADER_NAME: &str = "X-Headroom-Intercept-Identity";
pub const INTERCEPT_IDENTITY_HEADER_VALUE: &str = "headroom-intercept-v1";
pub const INTERCEPT_IDENTITY_BODY: &str = "headroom-intercept-v1";

/// Control endpoint for the detached helper. It is separate from the health
/// endpoint and requires the per-install token stored in the state file.
pub const ROUTER_CONTROL_PATH: &str = "/__headroom_codex_router_control";
pub const ROUTER_CONTROL_HEADER_NAME: &str = "X-Headroom-Codex-Router-Control";
pub const ROUTER_CONTROL_ACTION_HEADER_NAME: &str = "X-Headroom-Codex-Router-Action";
pub const ROUTER_CONTROL_ACTION_SHUTDOWN: &str = "shutdown";
pub const ROUTER_PROTOCOL_VERSION: &str = "2";
/// Advertise both the wire-protocol generation and the package version for
/// diagnostics. The build suffix also replaces an affected helper when a
/// locally rebuilt app retains the same package version.
pub const ROUTER_RUNTIME_VERSION: &str = concat!("2/", env!("CARGO_PKG_VERSION"), "+binary-head.1");

/// Argument understood by the desktop executable before Tauri startup.
pub const ROUTER_ARG: &str = "--headroom-codex-router";
const ROUTER_STATE_ENV: &str = "HEADROOM_CODEX_ROUTER_STATE";
const ROUTER_STATE_ARG: &str = "--headroom-codex-router-state";
const ROUTER_HEALTH_PATH: &str = "/__headroom_codex_router_health";
const ROUTER_HEALTH_HEADER: &str = "X-Headroom-Codex-Router: 1";
/// Optional authentication sent by the desktop when it needs to decide
/// whether an existing listener is ours.  The ordinary health endpoint stays
/// public/readable (so users can still `curl` it), but a listener is reusable
/// only when it proves possession of the per-install state-file token.
const ROUTER_HEALTH_PROBE_HEADER_NAME: &str = "X-Headroom-Codex-Router-Probe";
const ROUTER_HEALTH_PROBE_ECHO_HEADER_NAME: &str = "X-Headroom-Codex-Router-Probe-Echo";
// Keep the probe marker byte-for-byte identical to the header emitted by the
// health response. `ensure_running` uses this marker to distinguish our
// listener from an unrelated process that happens to occupy 6891.
#[cfg(test)]
const ROUTER_HEALTH_MARKER: &[u8] = ROUTER_HEALTH_HEADER.as_bytes();
const ROUTER_VERSION_HEADER_NAME: &str = "X-Headroom-Codex-Router-Version";
const ROUTER_CONTROL_TOKEN_KEY: &str = "control=";

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
// Two missed ticks are enough to distinguish a live GUI from a process that
// was killed, while leaving room for a busy event loop or a short disk stall.
const HEARTBEAT_STALE_AFTER: Duration = Duration::from_secs(4);
const STARTUP_WAIT: Duration = Duration::from_secs(3);
const STARTUP_POLL: Duration = Duration::from_millis(50);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const BODY_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 100 * 1024 * 1024;
const MAX_INFLIGHT: usize = 256;
const ROUTER_SHUTDOWN_DRAIN: Duration = Duration::from_millis(100);
const ROUTER_SHUTDOWN_WAIT: Duration = Duration::from_secs(3);

const OPENAI_BASE: &str = "https://api.openai.com";
const CHATGPT_CODEX_BASE: &str = "https://chatgpt.com/backend-api/codex";
pub(crate) const CHATGPT_CODEX_PATH_PREFIX: &str = "/backend-api/codex";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteMode {
    Proxy,
    Direct,
}

/// Pick the local provider base that preserves Codex's native URL shape for
/// the active credential.  OAuth sessions use `/backend-api/codex`; API-key
/// sessions use the public-compatible `/v1` route.
pub(crate) fn codex_router_base_url(chatgpt_auth: bool) -> &'static str {
    if chatgpt_auth {
        CODEX_ROUTER_CHATGPT_BASE_URL
    } else {
        CODEX_ROUTER_OPENAI_BASE_URL
    }
}

impl RouteMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Proxy => "proxy",
            Self::Direct => "direct",
        }
    }
}

/// The process-local heartbeat stop flag.  The sidecar never uses this slot;
/// it observes the state file instead, so the sidecar has no dependency on
/// the GUI's address space.
static HEARTBEAT_STOP: OnceLock<Mutex<Option<Arc<AtomicBool>>>> = OnceLock::new();
static UPSTREAM_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
static STATE_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static START_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
// Serializes lifecycle transitions and gives each heartbeat thread a
// generation.  Without the generation check, a thread that woke just as
// `stop_heartbeat` wrote `direct` could write `proxy` back afterwards.
static HEARTBEAT_LIFECYCLE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static HEARTBEAT_GENERATION: AtomicU64 = AtomicU64::new(0);
static ROUTER_SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

fn heartbeat_slot() -> &'static Mutex<Option<Arc<AtomicBool>>> {
    HEARTBEAT_STOP.get_or_init(|| Mutex::new(None))
}

fn state_write_lock() -> &'static Mutex<()> {
    STATE_WRITE_LOCK.get_or_init(|| Mutex::new(()))
}

fn start_lock() -> &'static Mutex<()> {
    START_LOCK.get_or_init(|| Mutex::new(()))
}

fn heartbeat_lifecycle_lock() -> &'static Mutex<()> {
    HEARTBEAT_LIFECYCLE_LOCK.get_or_init(|| Mutex::new(()))
}

fn upstream_client() -> &'static reqwest::Client {
    UPSTREAM_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            // Codex's realtime transport uses HTTP/1.1 Upgrade.  Restricting
            // this small client avoids ALPN selecting HTTP/2, where an
            // `Upgrade: websocket` request cannot be forwarded as a raw
            // bidirectional stream.
            .http1_only()
            .build()
            .expect("Codex router HTTP client")
    })
}

/// Return the per-user control file shared by the GUI and detached router.
/// `HEADROOM_CODEX_ROUTER_STATE` is intentionally supported for tests and for
/// packaged environments whose writable data directory is redirected.
pub fn state_path() -> PathBuf {
    if let Ok(path) = std::env::var(ROUTER_STATE_ENV) {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    // Match the app-data resolver used by every other Headroom receipt.  This
    // keeps the GUI and detached child on the same path on macOS/Windows and
    // honors `HEADROOM_DATA_DIR` in tests and redirected installations.
    crate::storage::config_file(&crate::storage::app_data_dir(), "codex-router.state")
}

/// Start (or reuse) the detached listener.  Unknown software already holding
/// the port is never killed or overwritten; in that case the caller can leave
/// Codex on its native provider URL.
pub fn ensure_running() -> std::io::Result<()> {
    let _start_guard = start_lock().lock();
    // Create the per-install control token before probing/spawning.  The token
    // is retained by every subsequent heartbeat write, and is never placed in
    // argv where another local process could read it from `ps`.
    let control_token = ensure_control_token()?;
    if probe_router_public() {
        // Never reuse a listener merely because it copied our public health
        // marker.  The detached helper must echo the token it read from the
        // per-install state file; an old helper or unrelated process is
        // treated as an occupied port and reported to the caller before any
        // Codex config is written.
        match probe_router_owned_version(&control_token) {
            Some(version) if version == ROUTER_RUNTIME_VERSION => return Ok(()),
            Some(version) if router_version_compatible(&version) => {
                // Only an authenticated, same-protocol helper may be updated.
                // Keeping an old helper alive also keeps its routing bugs alive
                // after the GUI is upgraded. Never resolve/kill an arbitrary PID.
                shutdown_and_wait_locked()?;
            }
            Some(_) => {
                // A listener that proves ownership but advertises another
                // protocol generation cannot safely be reused.  Do not send
                // an authenticated shutdown request to it; callers can
                // surface this error and leave the user's native Codex route
                // intact.
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Codex router protocol version is incompatible",
                ));
            }
            None => return Err(router_listener_unverified_error()),
        }
    }

    let executable = std::env::current_exe()?;
    let state = state_path();
    let mut command = crate::proc::command(executable);
    command
        .arg(ROUTER_ARG)
        .arg(ROUTER_STATE_ARG)
        .arg(&state)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // A separate process group prevents a GUI process-group teardown from
    // taking the listener with it.  On macOS/Linux an ordinary child already
    // survives a parent crash; this additionally covers explicit group kills.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS.  The latter keeps the
        // router out of the GUI console/job lifetime on Windows; the former
        // lets a parent group shutdown leave this helper running.
        command.creation_flags(0x0000_0200 | 0x0000_0008);
    }
    command.spawn()?;

    let deadline = std::time::Instant::now() + STARTUP_WAIT;
    while std::time::Instant::now() < deadline {
        if probe_router_owned(&control_token) {
            return Ok(());
        }
        // If something else claimed the port while our child was starting,
        // fail early instead of waiting for the full startup timeout.  This
        // keeps setup from writing a 6891 route that cannot be owned by us.
        if probe_router_public() {
            if let Some(version) = probe_router_owned_version(&control_token) {
                if version == ROUTER_RUNTIME_VERSION {
                    return Ok(());
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Codex router protocol version is incompatible",
                ));
            }
            return Err(router_listener_unverified_error());
        }
        std::thread::sleep(STARTUP_POLL);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("Codex router did not open 127.0.0.1:{CODEX_ROUTER_PORT}"),
    ))
}

/// Ask an existing detached router to terminate through its authenticated
/// local control endpoint, then wait until the port is released. A listener
/// that does not present our exact health marker is never touched.
pub fn shutdown_and_wait() -> std::io::Result<()> {
    let _start_guard = start_lock().lock();
    shutdown_and_wait_locked()
}

fn shutdown_and_wait_locked() -> std::io::Result<()> {
    if !probe_router_public() {
        return Ok(());
    }
    let Some(token) = current_control_token() else {
        // A marked listener is present but this install has no token with
        // which to prove ownership.  Never send control data to it.
        return Err(router_listener_unverified_error());
    };
    if probe_router_owned_version(&token).is_none() {
        return Err(router_listener_unverified_error());
    }
    if !request_shutdown(&token) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Codex router rejected authenticated shutdown",
        ));
    }
    wait_router_released()
}

fn router_listener_unverified_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!("127.0.0.1:{CODEX_ROUTER_PORT} is occupied by an unverified Codex router listener"),
    )
}

fn wait_router_released() -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + ROUTER_SHUTDOWN_WAIT;
    while std::time::Instant::now() < deadline {
        if !probe_router_public() {
            return Ok(());
        }
        std::thread::sleep(STARTUP_POLL);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("Codex router did not release 127.0.0.1:{CODEX_ROUTER_PORT}"),
    ))
}

fn request_shutdown(token: &str) -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], CODEX_ROUTER_PORT));
    let Ok(mut stream) = StdTcpStream::connect_timeout(&address, CONNECT_TIMEOUT) else {
        return false;
    };
    let request = format!(
        "POST {ROUTER_CONTROL_PATH} HTTP/1.1\r\nHost: 127.0.0.1:{CODEX_ROUTER_PORT}\r\n{ROUTER_CONTROL_HEADER_NAME}: {token}\r\n{ROUTER_CONTROL_ACTION_HEADER_NAME}: {ROUTER_CONTROL_ACTION_SHUTDOWN}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let _ = stream.set_read_timeout(Some(CONNECT_TIMEOUT));
    let mut response = Vec::with_capacity(256);
    let mut chunk = [0u8; 256];
    while response.len() < 4096 {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(size) => {
                response.extend_from_slice(&chunk[..size]);
                if response.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => return false,
        }
    }
    let Ok(text) = std::str::from_utf8(&response) else {
        return false;
    };
    text.lines().next().is_some_and(|line| {
        line.split_whitespace()
            .nth(1)
            .is_some_and(|status| status == "202")
    })
}

/// Mark the router as active and keep the heartbeat fresh until
/// [`stop_heartbeat`] is called.  The first write happens synchronously so a
/// just-configured Codex request cannot race a missing state file.
pub fn start_heartbeat() -> std::io::Result<()> {
    let _lifecycle_guard = heartbeat_lifecycle_lock().lock();
    stop_heartbeat_locked();
    write_state(RouteMode::Proxy)?;
    let generation = HEARTBEAT_GENERATION.load(Ordering::Acquire);

    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let spawn_result = std::thread::Builder::new()
        .name("codex-router-heartbeat".into())
        .spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                std::thread::sleep(HEARTBEAT_INTERVAL);
                if thread_stop.load(Ordering::Acquire)
                    || HEARTBEAT_GENERATION.load(Ordering::Acquire) != generation
                {
                    break;
                }
                if let Err(error) = write_state_if_generation(RouteMode::Proxy, generation) {
                    // A failed heartbeat is deliberately not retried in a
                    // tight loop.  The old mtime expires and the sidecar falls
                    // back to direct traffic automatically.
                    log::warn!("codex router heartbeat failed: {error}");
                }
            }
        });
    match spawn_result {
        Ok(_handle) => {
            *heartbeat_slot().lock() = Some(stop);
            Ok(())
        }
        Err(error) => {
            // The synchronous proxy write above must not leave a permanent
            // proxy marker when the heartbeat thread could not be created.
            HEARTBEAT_GENERATION.fetch_add(1, Ordering::AcqRel);
            stop_heartbeat_locked();
            Err(error)
        }
    }
}

/// Switch to native forwarding before the GUI tears down its intercept.  The
/// detached listener remains alive so existing Codex processes keep using the
/// same URL and do not need a restart.
pub fn stop_heartbeat() {
    let _lifecycle_guard = heartbeat_lifecycle_lock().lock();
    stop_heartbeat_locked();
}

fn stop_heartbeat_locked() {
    HEARTBEAT_GENERATION.fetch_add(1, Ordering::AcqRel);
    if let Some(stop) = heartbeat_slot().lock().take() {
        stop.store(true, Ordering::Release);
    }
    // A missing marker already means Direct to the detached listener.  Avoid
    // creating a control file from unit-test/runtime teardown paths that never
    // started the listener; once a marker exists, always replace it so a live
    // Codex session can fail open before 6867 is torn down.
    if state_path().exists() {
        if let Err(error) = write_state(RouteMode::Direct) {
            log::warn!("codex router direct-mode marker failed: {error}");
            // The detached router interprets a missing or malformed marker as
            // Direct. If replacing the marker fails (read-only volume, stale
            // temp-file collision, transient filesystem error), removing the
            // old Proxy marker is the only safe fallback before 6867 is torn
            // down. Leaving it fresh would keep sending new Codex requests to
            // an intercept that the caller is about to stop.
            let path = state_path();
            if let Err(remove_error) = std::fs::remove_file(&path) {
                log::warn!(
                    "codex router could not remove stale proxy marker {} after direct write failed: {remove_error}",
                    path.display()
                );
            }
        }
    }
}

/// Explicit mode setter used by lifecycle code when it pauses/resumes the
/// backend without closing the desktop.  A direct mode transition also stops
/// the heartbeat; otherwise its next tick would silently switch the router
/// back to proxy mode.  Conversely, selecting proxy mode owns a fresh
/// heartbeat so callers cannot accidentally publish a marker that expires.
pub fn set_mode(mode: RouteMode) -> std::io::Result<()> {
    match mode {
        RouteMode::Proxy => start_heartbeat(),
        RouteMode::Direct => {
            stop_heartbeat();
            Ok(())
        }
    }
}

/// Run the detached listener when the executable was launched with
/// [`ROUTER_ARG`].  Returns `true` for the sidecar process; the caller should
/// return from the normal Tauri `run` function immediately in that case.
pub fn run_if_requested() -> bool {
    let mut args = std::env::args_os();
    let requested = args.any(|arg| arg == ROUTER_ARG);
    if !requested {
        return false;
    }

    // Keep test/packager overrides in sync when a parent passed an explicit
    // state path.  This is not accepted from network input; it is a local
    // command-line argument emitted only by ensure_running().
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == ROUTER_STATE_ARG {
            if let Some(path) = args.next() {
                std::env::set_var(ROUTER_STATE_ENV, path);
            }
        } else if let Some(path) = arg
            .to_string_lossy()
            .strip_prefix("--headroom-codex-router-state=")
        {
            std::env::set_var(ROUTER_STATE_ENV, path);
        }
    }

    let result = run_server();
    if let Err(error) = result {
        // The GUI redirects child stdio to null.  Logging is still useful in
        // debug builds, and never makes the sidecar panic on a broken pipe.
        log::error!("Codex router exited: {error}");
    }
    true
}

fn write_state(mode: RouteMode) -> std::io::Result<()> {
    let _write_guard = state_write_lock().lock();
    write_state_locked(mode)
}

/// Write a heartbeat only if its owner is still current.  The generation is
/// checked while holding the same lock used by `stop_heartbeat_locked`, so a
/// stale thread can never overwrite the direct marker after shutdown.
fn write_state_if_generation(mode: RouteMode, generation: u64) -> std::io::Result<()> {
    let _write_guard = state_write_lock().lock();
    if HEARTBEAT_GENERATION.load(Ordering::Acquire) != generation {
        return Ok(());
    }
    write_state_locked(mode)
}

fn write_state_locked(mode: RouteMode) -> std::io::Result<()> {
    let control = read_control_token(&state_path()).unwrap_or_else(generate_control_token);
    write_state_locked_with_control(mode, &control)
}

fn write_state_locked_with_control(mode: RouteMode, control: &str) -> std::io::Result<()> {
    let path = state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Use the repository's shared tmp+rename writer. The state carries the
    // router control token, and a direct truncate fallback would violate the
    // persistence invariant as well as briefly expose a malformed record to
    // the detached reader.
    let content = format!(
        "mode={}\nheartbeat={}\nrouter_protocol={}\nrouter_version={}\ncontrol={}\n",
        mode.as_str(),
        unix_millis(),
        ROUTER_PROTOCOL_VERSION,
        ROUTER_RUNTIME_VERSION,
        control
    );
    crate::client_adapters::atomic_write(&path, content.as_bytes())
        .map_err(|error| std::io::Error::other(format!("writing Codex router state: {error:#}")))
}

/// Read the control token without creating or mutating the state file. A
/// token is accepted only when it has the expected UUID-like shape; malformed
/// values are treated as absent and replaced on the next state write.
fn read_control_token(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let token = raw
        .lines()
        .find_map(|line| line.strip_prefix(ROUTER_CONTROL_TOKEN_KEY))?
        .trim();
    if token.len() < 32
        || token.len() > 128
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return None;
    }
    Some(token.to_string())
}

fn generate_control_token() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Ensure a token exists, returning it for authenticated control requests.
/// This function serializes with heartbeat writes so a sidecar can never read
/// a half-written state record.
fn ensure_control_token() -> std::io::Result<String> {
    let _write_guard = state_write_lock().lock();
    let path = state_path();
    if let Some(token) = read_control_token(&path) {
        return Ok(token);
    }
    let token = generate_control_token();
    write_state_locked_with_control(RouteMode::Direct, &token)?;
    Ok(token)
}

fn current_control_token() -> Option<String> {
    let _write_guard = state_write_lock().lock();
    read_control_token(&state_path())
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn read_state(path: &Path) -> Option<(RouteMode, SystemTime)> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    if age > HEARTBEAT_STALE_AFTER {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let mode = text.lines().find_map(|line| {
        let value = line.strip_prefix("mode=")?;
        match value.trim() {
            "proxy" => Some(RouteMode::Proxy),
            "direct" => Some(RouteMode::Direct),
            _ => None,
        }
    })?;
    Some((mode, modified))
}

fn current_mode() -> RouteMode {
    read_state(&state_path())
        .map(|(mode, _)| mode)
        .unwrap_or(RouteMode::Direct)
}

/// Return true only for the exact identity response emitted by Headroom's
/// in-process intercept.  A generic HTTP response (including the old `/health`
/// probe) is deliberately insufficient: another loopback service could answer
/// it and receive Codex credentials when the router is in proxy mode.
fn intercept_identity_matches(response: &[u8]) -> bool {
    let Some(end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let Ok(head) = std::str::from_utf8(&response[..end]) else {
        return false;
    };
    let mut status_ok = false;
    let mut identity_ok = false;
    let mut content_length = None;
    for (index, line) in head.split("\r\n").enumerate() {
        if index == 0 {
            status_ok = line
                .split_whitespace()
                .nth(1)
                .is_some_and(|status| status == "200");
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case(INTERCEPT_IDENTITY_HEADER_NAME) {
            identity_ok = value.trim() == INTERCEPT_IDENTITY_HEADER_VALUE;
        }
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    if !status_ok || !identity_ok {
        return false;
    }
    let body = &response[end + 4..];
    // Require the marker body as well as the header. This avoids accepting a
    // proxy that copied only the header name/value from a diagnostic page.
    body == INTERCEPT_IDENTITY_BODY.as_bytes()
        && content_length == Some(INTERCEPT_IDENTITY_BODY.len())
}

fn probe_intercept_identity_blocking() -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], INTERCEPT_PORT));
    let Ok(mut stream) = StdTcpStream::connect_timeout(&address, Duration::from_millis(500)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    let request = format!(
        "GET {INTERCEPT_IDENTITY_PATH} HTTP/1.1\r\nHost: 127.0.0.1:{INTERCEPT_PORT}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = Vec::with_capacity(256);
    let mut chunk = [0u8; 512];
    while response.len() < 4096 {
        let Ok(size) = stream.read(&mut chunk) else {
            return false;
        };
        if size == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..size]);
        if response.windows(4).any(|window| window == b"\r\n\r\n") {
            // The endpoint body is fixed and tiny; one additional read is
            // enough for packet fragmentation while avoiding an unbounded
            // read from a foreign listener.
            if response.len() >= 256 || response.ends_with(INTERCEPT_IDENTITY_BODY.as_bytes()) {
                break;
            }
        }
    }
    intercept_identity_matches(&response)
}

/// Synchronous identity gate used by lifecycle code before publishing a fresh
/// proxy heartbeat. A pause/resume transition always verifies the current
/// owner of :6867.
pub(crate) fn intercept_is_headroom() -> bool {
    probe_intercept_identity_blocking()
}

async fn probe_intercept_identity_async() -> bool {
    let connect = TcpStream::connect(("127.0.0.1", INTERCEPT_PORT));
    let Ok(Ok(mut stream)) = tokio::time::timeout(Duration::from_millis(500), connect).await else {
        return false;
    };
    let request = format!(
        "GET {INTERCEPT_IDENTITY_PATH} HTTP/1.1\r\nHost: 127.0.0.1:{INTERCEPT_PORT}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).await.is_err() {
        return false;
    }
    let mut response = Vec::with_capacity(256);
    let mut chunk = [0u8; 512];
    loop {
        let Ok(Ok(size)) =
            tokio::time::timeout(Duration::from_millis(500), stream.read(&mut chunk)).await
        else {
            return false;
        };
        if size == 0 || response.len() >= 4096 {
            break;
        }
        response.extend_from_slice(&chunk[..size]);
        if intercept_identity_matches(&response) {
            return true;
        }
    }
    intercept_identity_matches(&response)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RouterHealthInfo {
    status: u16,
    marker: Option<String>,
    version: Option<String>,
    probe_echo: Option<String>,
}

/// Parse the small HTTP response emitted by the router health endpoint.  Keep
/// this independent of a socket so ownership checks can be unit-tested with
/// fixtures and so duplicate/conflicting headers are rejected deterministically.
fn parse_router_health_response(response: &[u8]) -> Option<RouterHealthInfo> {
    let end = find_header_end(response)?;
    let text = std::str::from_utf8(&response[..end]).ok()?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next()?;
    let mut status_parts = status_line.split_whitespace();
    let protocol = status_parts.next()?;
    if !protocol.starts_with("HTTP/") {
        return None;
    }
    let status = status_parts.next()?.parse::<u16>().ok()?;
    let mut marker = None;
    let mut version = None;
    let mut probe_echo = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        let value = value.trim().to_string();
        let slot = if name.eq_ignore_ascii_case("X-Headroom-Codex-Router") {
            &mut marker
        } else if name.eq_ignore_ascii_case(ROUTER_VERSION_HEADER_NAME) {
            &mut version
        } else if name.eq_ignore_ascii_case(ROUTER_HEALTH_PROBE_ECHO_HEADER_NAME) {
            &mut probe_echo
        } else {
            continue;
        };
        // Multiple copies of an ownership-sensitive header are ambiguous;
        // reject rather than accepting whichever value happened to be first.
        if slot.replace(value).is_some() {
            return None;
        }
    }
    Some(RouterHealthInfo {
        status,
        marker,
        version,
        probe_echo,
    })
}

fn router_health_public_matches(response: &[u8]) -> bool {
    parse_router_health_response(response)
        .is_some_and(|health| health.status == 200 && health.marker.as_deref() == Some("1"))
}

fn router_health_owned_version(response: &[u8], token: &str) -> Option<String> {
    let health = parse_router_health_response(response)?;
    if health.status != 200
        || health.marker.as_deref() != Some("1")
        || health
            .probe_echo
            .as_deref()
            .is_none_or(|echo| !constant_time_token_eq(echo, token))
    {
        return None;
    }
    health.version
}

/// Open a loopback health connection and read only through the end of the
/// response headers.  The endpoint's body is informational; status/marker/
/// version/probe echo are all carried in headers and can be validated without
/// waiting for a potentially misbehaving foreign listener to close the socket.
fn probe_router_response(token: Option<&str>) -> Option<Vec<u8>> {
    let address = SocketAddr::from(([127, 0, 0, 1], CODEX_ROUTER_PORT));
    let Ok(mut stream) = StdTcpStream::connect_timeout(&address, CONNECT_TIMEOUT) else {
        return None;
    };
    let _ = stream.set_read_timeout(Some(CONNECT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CONNECT_TIMEOUT));
    let probe = token
        .map(|token| format!("{ROUTER_HEALTH_PROBE_HEADER_NAME}: {token}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "GET {ROUTER_HEALTH_PATH} HTTP/1.1\r\nHost: 127.0.0.1:{CODEX_ROUTER_PORT}\r\n{probe}Connection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return None;
    }
    // A loopback response may be split across TCP packets. Read until the
    // complete header (or the small cap) so a short read cannot make us spawn
    // duplicate helpers while the existing one is alive.
    let mut response = Vec::with_capacity(256);
    let mut chunk = [0u8; 128];
    while response.len() < 4096 {
        let Ok(size) = stream.read(&mut chunk) else {
            return None;
        };
        if size == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..size]);
        if response.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    (!response.is_empty()).then_some(response)
}

fn probe_router_public() -> bool {
    probe_router_response(None).is_some_and(|response| router_health_public_matches(&response))
}

fn probe_router_owned_version(token: &str) -> Option<String> {
    let response = probe_router_response(Some(token))?;
    router_health_owned_version(&response, token)
}

/// Only the same wire protocol permits authenticated replacement of an old
/// helper. An incompatible generation fails closed without sending control
/// traffic to it, even if it presents the ownership token.
fn router_version_compatible(version: &str) -> bool {
    version
        .strip_prefix(ROUTER_PROTOCOL_VERSION)
        .is_some_and(|rest| rest.starts_with('/'))
}

fn probe_router_owned(token: &str) -> bool {
    probe_router_owned_version(token).as_deref() == Some(ROUTER_RUNTIME_VERSION)
}

#[cfg(test)]
fn probe_response_is_owned_current(response: &[u8], token: &str) -> bool {
    router_health_owned_version(response, token).as_deref() == Some(ROUTER_RUNTIME_VERSION)
}

fn run_server() -> std::io::Result<()> {
    ROUTER_SHUTDOWN_REQUESTED.store(false, Ordering::Release);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    runtime.block_on(async {
        let address = SocketAddr::from(([127, 0, 0, 1], CODEX_ROUTER_PORT));
        let listener = TcpListener::bind(address).await?;
        let permits = Arc::new(tokio::sync::Semaphore::new(MAX_INFLIGHT));
        loop {
            if ROUTER_SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
                break;
            }
            // A transient EMFILE/ECONNABORTED from the loopback listener must
            // not kill the detached helper.  The GUI may be gone at this point,
            // so there is no later call to `ensure_running` that could revive it.
            match tokio::time::timeout(Duration::from_millis(100), listener.accept()).await {
                Ok(Ok((client, _))) => {
                    let permits = permits.clone();
                    tokio::spawn(async move {
                        let Ok(_permit) = permits.acquire_owned().await else {
                            return;
                        };
                        handle_client(client).await;
                    });
                }
                Ok(Err(error)) => {
                    if ROUTER_SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
                        break;
                    }
                    log::debug!("codex router accept failed; keeping listener alive: {error}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(_) => {
                    // Periodically wake so an authenticated shutdown request
                    // can terminate even when no new connection arrives.
                }
            }
        }
        Ok(())
    })
}

async fn handle_client(mut client: TcpStream) {
    let mut header_buf = Vec::with_capacity(4096);
    let read = tokio::time::timeout(
        HEADER_TIMEOUT,
        read_http_headers(&mut client, &mut header_buf),
    )
    .await;
    if !matches!(read, Ok(Ok(()))) {
        return;
    }
    if !request_is_loopback_safe(&header_buf) {
        let _ = client
            .write_all(
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await;
        return;
    }
    if is_router_control_request(&header_buf) {
        handle_router_control(&mut client, &header_buf).await;
        return;
    }
    if is_router_health_request(&header_buf) {
        let mode = current_mode();
        let body = mode.as_str();
        // Public health remains readable without credentials.  When the
        // desktop includes the probe header, echo it only after validating the
        // value against the token in the state file.  This lets the desktop
        // distinguish our detached helper from an unrelated listener on the
        // same port without exposing the token in ordinary curl output.
        let probe_echo = parse_request_head(&header_buf)
            .and_then(|request| {
                header_value(&request.headers, ROUTER_HEALTH_PROBE_HEADER_NAME).map(str::to_owned)
            })
            .and_then(|supplied| {
                current_control_token()
                    .filter(|expected| constant_time_token_eq(supplied.trim(), expected.trim()))
            });
        let probe_header = probe_echo
            .as_deref()
            .map(|token| format!("{ROUTER_HEALTH_PROBE_ECHO_HEADER_NAME}: {token}\r\n"))
            .unwrap_or_default();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n{ROUTER_HEALTH_HEADER}\r\n{ROUTER_VERSION_HEADER_NAME}: {ROUTER_RUNTIME_VERSION}\r\n{probe_header}Connection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = client.write_all(response.as_bytes()).await;
        return;
    }

    let parsed = parse_request_head(&header_buf);
    let codex_path = parsed
        .as_ref()
        .is_some_and(|request| is_codex_path(&request.path));

    // A live Headroom intercept already performs request classification,
    // header stamping, usage accounting, and backend fallback.  Relaying the
    // bytes preserves all of that behaviour and keeps the sidecar tiny.
    if codex_path && current_mode() == RouteMode::Proxy && probe_intercept_identity_async().await {
        match TcpStream::connect(("127.0.0.1", INTERCEPT_PORT)).await {
            Err(_) => {
                // A refused connection means the intercept is already gone;
                // no request bytes reached it, so retrying natively is safe.
            }
            Ok(mut intercept) => {
                // ChatGPT OAuth providers address resources below
                // `/backend-api/codex`, while the in-process intercept uses
                // the OpenAI-compatible `/v1` form for classification. Bare
                // endpoint paths from older Codex builds are normalized too.
                // Keep the body bytes untouched so streaming/upgrade requests
                // retain their framing.
                let proxy_header = if codex_path {
                    let normalized = normalize_codex_path_for_intercept(
                        &parsed.as_ref().expect("codex_path implies parsed").path,
                    );
                    rewrite_request_target(&header_buf, &normalized)
                        .unwrap_or_else(|| header_buf.clone())
                } else {
                    header_buf.clone()
                };
                // Once even one byte has been accepted by the intercept we
                // must not replay the request against the native endpoint:
                // the backend may have processed it before the socket failed.
                // `write_all` either completes the header or leaves us on the
                // proxy path and the Codex client can retry at its own layer.
                if intercept.write_all(&proxy_header).await.is_err() {
                    return;
                }
                let _ = tokio::io::copy_bidirectional(&mut client, &mut intercept).await;
                return;
            }
        }
        // A process can die between the state read and the connect.  The
        // refused-connect case above falls through to the native path with
        // the untouched request bytes.
    }

    forward_direct(client, header_buf).await;
}

fn is_router_health_request(buf: &[u8]) -> bool {
    let Some(end) = find_header_end(buf) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&buf[..end]) else {
        return false;
    };
    text.lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .is_some_and(|path| path == ROUTER_HEALTH_PATH)
}

fn is_router_control_request(buf: &[u8]) -> bool {
    let Some(end) = find_header_end(buf) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&buf[..end]) else {
        return false;
    };
    let mut parts = text.lines().next().unwrap_or_default().split_whitespace();
    matches!(
        (parts.next(), parts.next()),
        (Some(method), Some(path)) if method.eq_ignore_ascii_case("POST") && path == ROUTER_CONTROL_PATH
    )
}

async fn handle_router_control(client: &mut TcpStream, header_buf: &[u8]) {
    let Some(end) = find_header_end(header_buf) else {
        write_simple(client, "400 Bad Request").await;
        return;
    };
    let Some(parsed) = parse_request_head(&header_buf[..end + 4]) else {
        write_simple(client, "400 Bad Request").await;
        return;
    };
    let supplied = header_value(&parsed.headers, ROUTER_CONTROL_HEADER_NAME).unwrap_or_default();
    let expected = current_control_token().unwrap_or_default();
    let action =
        header_value(&parsed.headers, ROUTER_CONTROL_ACTION_HEADER_NAME).unwrap_or_default();
    if expected.is_empty() || !constant_time_token_eq(supplied, &expected) {
        write_simple(client, "403 Forbidden").await;
        return;
    }
    if !action.eq_ignore_ascii_case(ROUTER_CONTROL_ACTION_SHUTDOWN) {
        write_simple(client, "400 Bad Request").await;
        return;
    }
    let response = format!(
        "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n{ROUTER_HEALTH_HEADER}\r\n{ROUTER_VERSION_HEADER_NAME}: {ROUTER_RUNTIME_VERSION}\r\nConnection: close\r\n\r\n"
    );
    if client.write_all(response.as_bytes()).await.is_err() {
        return;
    }
    // Let the caller receive the acknowledgement and give active relay tasks
    // a short chance to finish before the listener closes its runtime.
    tokio::time::sleep(ROUTER_SHUTDOWN_DRAIN).await;
    ROUTER_SHUTDOWN_REQUESTED.store(true, Ordering::Release);
}

fn constant_time_token_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut diff = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        diff |= left.get(index).copied().unwrap_or(0) as usize
            ^ right.get(index).copied().unwrap_or(0) as usize;
    }
    diff == 0
}

async fn forward_direct(mut client: TcpStream, header_buf: Vec<u8>) {
    let Some(header_end_pos) = find_header_end(&header_buf) else {
        write_simple(&mut client, "400 Bad Request").await;
        return;
    };
    let header_end = header_end_pos + 4;
    let Some(parsed) = parse_request_head(&header_buf[..header_end]) else {
        write_simple(&mut client, "400 Bad Request").await;
        return;
    };
    if !is_codex_path(&parsed.path) {
        write_simple(&mut client, "403 Forbidden").await;
        return;
    }

    // A stale local route can outlive the GUI after a user switches Codex to a
    // custom gateway. Never reinterpret that provider as OpenAI merely
    // because the detached router is in fail-open Direct mode. The helper
    // returns only the root provider identifier and keeps credentials/URLs out
    // of this process; an unknown provider gets a retryable response so Codex
    // can recover after Headroom is reopened or the user restores its config.
    if !crate::client_adapters::codex_root_provider_allows_native_fallback() {
        write_simple(&mut client, "503 Service Unavailable").await;
        return;
    }

    let chatgpt = request_uses_chatgpt_auth(&parsed.path, &parsed.headers);
    // Normalize both provider shapes before selecting the native upstream:
    // ChatGPT's base already contains `/backend-api/codex`, while the public
    // OpenAI base expects `/v1`. This also keeps older bare `/responses`
    // requests working when a user has an API-key session.
    let path = if chatgpt {
        normalize_chatgpt_path(&parsed.path)
    } else {
        normalize_codex_path_for_intercept(&parsed.path)
    };
    let base = if chatgpt {
        CHATGPT_CODEX_BASE
    } else {
        OPENAI_BASE
    };
    let url = format!("{base}{path}");
    let leftover = &header_buf[header_end..];

    // Clients using `Expect: 100-continue` wait for this interim response
    // before sending a potentially large JSON body.  Answer locally; the
    // upstream request is still issued only after the complete body arrives.
    if header_value(&parsed.headers, "expect")
        .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
        && client
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .await
            .is_err()
    {
        return;
    }

    let body = match read_request_body(&mut client, &parsed, leftover).await {
        Ok(body) => body,
        Err(BodyError::TooLarge) => {
            write_simple(&mut client, "413 Payload Too Large").await;
            return;
        }
        Err(BodyError::Malformed) => {
            write_simple(&mut client, "400 Bad Request").await;
            return;
        }
        Err(BodyError::Io) => return,
    };

    let method = match reqwest::Method::from_bytes(parsed.method.as_bytes()) {
        Ok(method) => method,
        Err(_) => {
            write_simple(&mut client, "400 Bad Request").await;
            return;
        }
    };
    let request_upgrade = header_value(&parsed.headers, "upgrade").is_some();
    if request_upgrade {
        tunnel_upgrade(&mut client, &parsed, body, &url).await;
        return;
    }

    let mut request = upstream_client().request(method, &url);
    for (name, value) in &parsed.headers {
        if is_hop_by_hop_request_header(name)
            || name.eq_ignore_ascii_case("host")
            || name.to_ascii_lowercase().starts_with("x-headroom-")
            || name.eq_ignore_ascii_case("x-openai-internal-codex-responses-lite")
        {
            continue;
        }
        request = request.header(name, value);
    }
    if !body.is_empty() {
        request = request.body(body);
    }

    let mut response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            log::debug!("codex router direct request failed: {error}");
            write_simple(&mut client, "502 Bad Gateway").await;
            return;
        }
    };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        response.status().as_u16(),
        response.status().canonical_reason().unwrap_or("")
    );
    for (name, value) in response.headers() {
        if is_hop_by_hop_response_header(name.as_str()) {
            continue;
        }
        if let Ok(value) = value.to_str() {
            head.push_str(name.as_str());
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
    }
    head.push_str("Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
    if client.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    loop {
        match response.chunk().await {
            Ok(Some(bytes)) if !bytes.is_empty() => {
                let chunk_head = format!("{:X}\r\n", bytes.len());
                if client.write_all(chunk_head.as_bytes()).await.is_err()
                    || client.write_all(&bytes).await.is_err()
                    || client.write_all(b"\r\n").await.is_err()
                {
                    return;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(error) => {
                log::debug!("codex router direct response failed: {error}");
                return;
            }
        }
    }
    let _ = client.write_all(b"0\r\n\r\n").await;
}

async fn tunnel_upgrade(
    client: &mut TcpStream,
    parsed: &ParsedRequestHead,
    leftover: Vec<u8>,
    url: &str,
) {
    let Ok(method) = reqwest::Method::from_bytes(parsed.method.as_bytes()) else {
        write_simple(client, "400 Bad Request").await;
        return;
    };
    let mut request = upstream_client().request(method, url);
    for (name, value) in &parsed.headers {
        if name.eq_ignore_ascii_case("host")
            || name.eq_ignore_ascii_case("accept-encoding")
            || name.to_ascii_lowercase().starts_with("x-headroom-")
            || name.eq_ignore_ascii_case("x-openai-internal-codex-responses-lite")
            || matches!(
                name.to_ascii_lowercase().as_str(),
                "proxy-authorization" | "proxy-authenticate" | "te" | "trailers"
            )
        {
            continue;
        }
        request = request.header(name, value);
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            log::debug!("codex router direct upgrade failed: {error}");
            write_simple(client, "502 Bad Gateway").await;
            return;
        }
    };
    let status = response.status();
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    );
    for (name, value) in response.headers() {
        if name.as_str().eq_ignore_ascii_case("transfer-encoding") {
            continue;
        }
        if let Ok(value) = value.to_str() {
            head.push_str(name.as_str());
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
    }
    head.push_str("\r\n");
    if status != reqwest::StatusCode::SWITCHING_PROTOCOLS {
        let body = response.bytes().await.unwrap_or_default();
        if client.write_all(head.as_bytes()).await.is_ok() {
            let _ = client.write_all(&body).await;
        }
        return;
    }
    let mut upstream = match response.upgrade().await {
        Ok(upstream) => upstream,
        Err(error) => {
            log::debug!("codex router upgrade takeover failed: {error}");
            write_simple(client, "502 Bad Gateway").await;
            return;
        }
    };
    if client.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    if !leftover.is_empty() && upstream.write_all(&leftover).await.is_err() {
        return;
    }
    let _ = tokio::io::copy_bidirectional(client, &mut upstream).await;
}

async fn write_simple(client: &mut TcpStream, status: &str) {
    let response = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let _ = client.write_all(response.as_bytes()).await;
}

enum BodyError {
    TooLarge,
    Malformed,
    Io,
}

async fn read_request_body(
    client: &mut TcpStream,
    parsed: &ParsedRequestHead,
    leftover: &[u8],
) -> Result<Vec<u8>, BodyError> {
    if parsed
        .content_length
        .is_some_and(|length| length > MAX_BODY_BYTES)
    {
        return Err(BodyError::TooLarge);
    }
    if parsed.content_length.is_none()
        && header_value(&parsed.headers, "transfer-encoding")
            .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    {
        return Err(BodyError::Malformed);
    }
    let Some(total) = parsed.content_length else {
        return Ok(leftover.to_vec());
    };
    if leftover.len() >= total {
        return Ok(leftover[..total].to_vec());
    }
    let mut body = Vec::with_capacity(total);
    body.extend_from_slice(leftover);
    let mut remaining = vec![0u8; total - leftover.len()];
    match tokio::time::timeout(BODY_TIMEOUT, client.read_exact(&mut remaining)).await {
        Ok(Ok(_)) => {
            body.extend_from_slice(&remaining);
            Ok(body)
        }
        _ => Err(BodyError::Io),
    }
}

struct ParsedRequestHead {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    content_length: Option<usize>,
}

fn parse_request_head(buf: &[u8]) -> Option<ParsedRequestHead> {
    // A socket read can include binary body bytes after the headers (Codex
    // uses zstd). Those bytes must not turn a proxy request into Direct mode.
    let end = find_header_end(buf).unwrap_or(buf.len());
    let text = std::str::from_utf8(&buf[..end]).ok()?;
    let mut lines = text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    if parts.next().is_none() {
        return None;
    }
    let mut headers = Vec::new();
    let mut content_length = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        let name = name.trim().to_string();
        let value = value.trim().to_string();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(value.parse().ok()?);
        }
        headers.push((name, value));
    }
    Some(ParsedRequestHead {
        method,
        path,
        headers,
        content_length,
    })
}

async fn read_http_headers<R>(stream: &mut R, buf: &mut Vec<u8>) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut chunk = [0u8; 4096];
    loop {
        let size = stream.read(&mut chunk).await?;
        if size == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "client closed connection",
            ));
        }
        buf.extend_from_slice(&chunk[..size]);
        if find_header_end(buf).is_some() {
            return Ok(());
        }
        if buf.len() > MAX_HEADER_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "headers exceed maximum size",
            ));
        }
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(field, _)| field.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn request_is_loopback_safe(buf: &[u8]) -> bool {
    let Some(end) = find_header_end(buf) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&buf[..end]) else {
        return false;
    };
    let mut host = None;
    for line in text.split("\r\n") {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("origin") {
                return false;
            }
            if name.eq_ignore_ascii_case("host") {
                host = Some(value.trim());
            }
        }
    }
    host.is_some_and(host_is_loopback)
}

fn host_is_loopback(host: &str) -> bool {
    let host = host
        .rsplit_once(':')
        .map(|(name, _)| name)
        .unwrap_or(host)
        .trim_start_matches('[')
        .trim_end_matches(']');
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// Remove the native ChatGPT route prefix when a provider base URL already
/// contains it. The boundary check prevents `/backend-api/codexevil` from
/// being treated as a valid Codex target.
fn strip_chatgpt_codex_prefix(path: &str) -> String {
    let Some(rest) = path.strip_prefix(CHATGPT_CODEX_PATH_PREFIX) else {
        return path.to_string();
    };
    if rest.is_empty() {
        "/".to_string()
    } else if rest.starts_with('/') {
        rest.to_string()
    } else if rest.starts_with('?') {
        format!("/{rest}")
    } else {
        path.to_string()
    }
}

/// Canonical local/intercept form. Codex's public provider emits `/v1/*`,
/// while its ChatGPT provider emits `*/backend-api/codex/*`. Keep one form on
/// the 6867 wire so request classification and backend routing stay identical
/// for both auth modes. Bare endpoint paths are accepted for older clients.
pub(crate) fn normalize_codex_path_for_intercept(path: &str) -> String {
    let path = strip_chatgpt_codex_prefix(path);
    if path == "/v1"
        || path.starts_with("/v1/")
        || path.starts_with("/v1?")
        || !path.starts_with('/')
    {
        path
    } else {
        format!("/v1{path}")
    }
}

/// Native ChatGPT endpoint form. The upstream base already ends in
/// `/backend-api/codex`, so strip both the local compatibility prefix and the
/// optional public `/v1` prefix before constructing the URL.
pub(crate) fn normalize_chatgpt_path(path: &str) -> String {
    let path = strip_chatgpt_codex_prefix(path);
    if path == "/v1" {
        return "/".to_string();
    }
    path.strip_prefix("/v1")
        .filter(|rest| rest.starts_with('/') || rest.starts_with('?'))
        .map(|rest| {
            if rest.starts_with('?') {
                format!("/{rest}")
            } else {
                rest.to_string()
            }
        })
        .unwrap_or(path)
}

/// Rewrite a request target while preserving the header/body bytes around it.
/// Returns `None` for an invalid request line; callers then use their normal
/// malformed-request response instead of guessing at byte offsets.
pub(crate) fn rewrite_request_target(buf: &[u8], new_path: &str) -> Option<Vec<u8>> {
    let line_end = buf.windows(2).position(|window| window == b"\r\n")?;
    let line = &buf[..line_end];
    let first_space = line.iter().position(|byte| *byte == b' ')?;
    let second_space = line[first_space + 1..]
        .iter()
        .position(|byte| *byte == b' ')
        .map(|offset| first_space + 1 + offset)?;
    let old_path = std::str::from_utf8(&line[first_space + 1..second_space]).ok()?;
    if old_path == new_path {
        return Some(buf.to_vec());
    }
    let mut rewritten = Vec::with_capacity(buf.len() + new_path.len() - old_path.len());
    rewritten.extend_from_slice(&buf[..first_space + 1]);
    rewritten.extend_from_slice(new_path.as_bytes());
    rewritten.extend_from_slice(&buf[second_space..]);
    Some(rewritten)
}

fn is_codex_path(path: &str) -> bool {
    // Keep an origin-form request target. Allow the complete known
    // OpenAI/Codex path family, including the `/backend-api/codex` form used
    // by ChatGPT OAuth. The upstream host is fixed below and is never taken
    // from the request, so this remains a local allowlist.
    let path = normalize_codex_path_for_intercept(path);
    if !path.starts_with('/') || path.starts_with("//") {
        return false;
    }
    const PREFIXES: &[&str] = &[
        "/v1/responses",
        "/v1/chat/completions",
        "/v1/completions",
        "/v1/embeddings",
        "/v1/models",
        "/v1/images",
        "/v1/audio",
        "/v1/files",
        "/v1/batches",
        "/v1/fine_tuning",
        "/v1/vector_stores",
        "/v1/threads",
        "/v1/assistants",
        "/v1/realtime",
        "/v1/live",
        "/v1/memories",
        "/v1/guardian",
        "/v1/guardian-classifier",
        "/v1/alpha/search",
        "/v1/connectors",
    ];
    PREFIXES.iter().any(|prefix| {
        path.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || rest.starts_with('?'))
    })
}

/// Crate-visible classifier used by the in-process 6867 intercept before it
/// rewrites a ChatGPT-shaped request target to the canonical `/v1` form.
pub(crate) fn is_codex_path_compat(path: &str) -> bool {
    is_codex_path(path)
}

fn request_uses_chatgpt_auth(path: &str, headers: &[(String, String)]) -> bool {
    // An explicit API-key header must win over a stale OAuth profile on disk.
    if header_value(headers, "x-api-key").is_some_and(|value| !value.trim().is_empty())
        || header_value(headers, "api-key").is_some_and(|value| !value.trim().is_empty())
    {
        return false;
    }
    if let Some(value) = header_value(headers, "authorization") {
        if let Some((scheme, token)) = value.split_once(char::is_whitespace) {
            if scheme.eq_ignore_ascii_case("bearer") {
                let token = token.trim();
                // OpenAI Platform keys are deliberately never sent to the
                // ChatGPT backend, even when auth.json still records a
                // previous subscription.
                if token.starts_with("sk-") || token.starts_with("sk-proj-") {
                    return false;
                }
                if decode_auth_claim(token, "chatgpt_account_id").is_some() {
                    return true;
                }
            }
        }
    }
    // The managed ChatGPT provider deliberately includes this suffix in its
    // stable local base URL. It is therefore strong route intent even when a
    // host integration supplies an opaque bearer without a decodable account
    // claim. Keep the explicit API-key checks above so a stale ChatGPT-shaped
    // URL can never send a Platform key to the ChatGPT backend.
    if strip_chatgpt_codex_prefix(path) != path {
        return true;
    }
    if header_value(headers, "chatgpt-account-id").is_some_and(|value| !value.trim().is_empty()) {
        return true;
    }

    // Some Codex desktop builds send an opaque OAuth bearer without the account
    // header and without a decodable JWT. Fall back to Codex's local auth
    // profile so those credentials do not get misrouted to api.openai.com.
    auth_file_indicates_chatgpt()
}

pub(crate) fn auth_file_indicates_chatgpt() -> bool {
    let path = std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join(".codex")
        })
        .join("auth.json");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    if let Some(mode) = object.get("auth_mode").and_then(serde_json::Value::as_str) {
        let normalized: String = mode
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        if normalized == "apikey" || normalized == "codexapikey" {
            return false;
        }
        if normalized.starts_with("chatgpt")
            || matches!(
                normalized.as_str(),
                "headers" | "agentidentity" | "personalaccesstoken"
            )
        {
            return true;
        }
    }
    let tokens = object.get("tokens").and_then(serde_json::Value::as_object);
    if tokens
        .and_then(|tokens| tokens.get("account_id"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|id| !id.trim().is_empty())
    {
        return true;
    }
    let id_token = tokens
        .and_then(|tokens| tokens.get("id_token"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| object.get("id_token").and_then(serde_json::Value::as_str));
    if id_token
        .and_then(|token| decode_auth_claim(token, "chatgpt_account_id"))
        .is_some_and(|id| !id.trim().is_empty())
    {
        return true;
    }

    // Headless integrations can expose an opaque OAuth token without an
    // auth.json file or account header. An explicit API key still wins.
    std::env::var("CODEX_ACCESS_TOKEN")
        .ok()
        .is_some_and(|token| !token.trim().is_empty())
        && !std::env::var("OPENAI_API_KEY")
            .ok()
            .is_some_and(|key| !key.trim().is_empty())
}

fn decode_auth_claim(token: &str, claim: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?.trim_end_matches('=');
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let value = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
    value
        .get("https://api.openai.com/auth")?
        .get(claim)?
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn is_hop_by_hop_request_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "transfer-encoding"
            | "te"
            | "trailers"
            | "proxy-authorization"
            | "proxy-authenticate"
            | "upgrade"
            | "host"
            | "content-length"
            | "accept-encoding"
    )
}

fn is_hop_by_hop_response_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "transfer-encoding"
            | "te"
            | "trailers"
            | "proxy-authorization"
            | "proxy-authenticate"
            | "upgrade"
            | "content-length"
            | "content-encoding"
    )
}

// Keep a small pure parser surface for unit tests and for callers that want
// to display the current route without opening the listener.
pub fn route_mode() -> RouteMode {
    current_mode()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chatgpt_path_drops_openai_v1_prefix() {
        assert_eq!(normalize_chatgpt_path("/v1/responses"), "/responses");
        assert_eq!(normalize_chatgpt_path("/v1/models?x=1"), "/models?x=1");
        assert_eq!(normalize_chatgpt_path("/responses"), "/responses");
        assert_eq!(
            normalize_chatgpt_path("/backend-api/codex/responses"),
            "/responses"
        );
        assert_eq!(
            normalize_chatgpt_path("/backend-api/codex/v1/models?x=1"),
            "/models?x=1"
        );
    }

    #[test]
    fn provider_path_normalization_targets_intercept_v1_shape() {
        assert_eq!(
            normalize_codex_path_for_intercept("/backend-api/codex/responses"),
            "/v1/responses"
        );
        assert_eq!(
            normalize_codex_path_for_intercept("/backend-api/codex/v1/models?x=1"),
            "/v1/models?x=1"
        );
        assert_eq!(
            normalize_codex_path_for_intercept("/responses?stream=true"),
            "/v1/responses?stream=true"
        );
        assert_eq!(
            normalize_codex_path_for_intercept("/v1/responses"),
            "/v1/responses"
        );
    }

    #[test]
    fn rewrite_request_target_preserves_body_and_http_version() {
        let request = b"POST /backend-api/codex/responses?stream=true HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}";
        let rewritten = rewrite_request_target(request, "/v1/responses?stream=true").unwrap();
        assert_eq!(
            rewritten,
            b"POST /v1/responses?stream=true HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}"
        );
    }

    #[test]
    fn api_paths_are_limited_to_codex_endpoints() {
        assert!(is_codex_path("/v1/responses"));
        assert!(is_codex_path("/v1/responses/compact"));
        assert!(is_codex_path("/v1/models?foo=bar"));
        assert!(is_codex_path("/memories/trace_summarize"));
        assert!(is_codex_path("/realtime/calls"));
        assert!(is_codex_path("/v1/guardian-classifier"));
        assert!(is_codex_path("/guardian-classifier"));
        assert!(is_codex_path("/v1/alpha/search"));
        assert!(is_codex_path("/alpha/search"));
        assert!(is_codex_path("/v1/live"));
        assert!(is_codex_path("/live"));
        assert!(is_codex_path("/backend-api/codex/responses"));
        assert!(is_codex_path("/backend-api/codex/alpha/search"));
        assert!(!is_codex_path("/backend-api/codex/messages"));
        assert!(!is_codex_path("/v1/messages"));
        assert!(!is_codex_path("/stats"));
        assert!(!is_codex_path("http://evil.example/v1/responses"));
    }

    #[test]
    fn chatgpt_auth_is_detected_from_account_header() {
        let headers = vec![("ChatGPT-Account-ID".to_string(), "acct".to_string())];
        assert!(request_uses_chatgpt_auth("/v1/responses", &headers));
        let api_headers = vec![("Authorization".to_string(), "Bearer sk-test".to_string())];
        assert!(!request_uses_chatgpt_auth("/v1/responses", &api_headers));
        let mixed_headers = vec![
            ("ChatGPT-Account-ID".to_string(), "stale-acct".to_string()),
            ("X-Api-Key".to_string(), "sk-test".to_string()),
        ];
        assert!(
            !request_uses_chatgpt_auth("/v1/responses", &mixed_headers),
            "an explicit API key must take precedence over a stale account header"
        );
    }

    #[test]
    fn api_key_header_and_platform_bearer_override_account_header() {
        let api_key = vec![
            ("ChatGPT-Account-ID".to_string(), "stale-acct".to_string()),
            ("api-key".to_string(), "sk-test".to_string()),
        ];
        assert!(!request_uses_chatgpt_auth(
            "/backend-api/codex/responses",
            &api_key
        ));

        let bearer = vec![
            ("ChatGPT-Account-ID".to_string(), "stale-acct".to_string()),
            (
                "Authorization".to_string(),
                "Bearer sk-proj-test".to_string(),
            ),
        ];
        assert!(!request_uses_chatgpt_auth(
            "/backend-api/codex/responses",
            &bearer
        ));
    }

    #[test]
    fn chatgpt_auth_is_detected_from_jwt_account_claim() {
        let payload = serde_json::json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct_jwt"}
        });
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string());
        let token = format!("e30.{encoded}.sig");
        let headers = vec![("Authorization".to_string(), format!("Bearer {token}"))];
        assert!(request_uses_chatgpt_auth("/v1/responses", &headers));
    }

    #[test]
    fn chatgpt_provider_path_routes_opaque_bearer_to_chatgpt() {
        let headers = vec![(
            "Authorization".to_string(),
            "Bearer opaque-host-token".to_string(),
        )];
        assert!(request_uses_chatgpt_auth(
            "/backend-api/codex/responses",
            &headers
        ));
    }

    #[test]
    fn loopback_guard_rejects_browser_origin() {
        let good = b"GET /v1/models HTTP/1.1\r\nHost: 127.0.0.1:6891\r\n\r\n";
        assert!(request_is_loopback_safe(good));
        let browser = b"GET /v1/models HTTP/1.1\r\nHost: 127.0.0.1:6891\r\nOrigin: https://evil.example\r\n\r\n";
        assert!(!request_is_loopback_safe(browser));
    }

    #[test]
    fn codex_binary_body_does_not_bypass_proxy_classification() {
        let mut request = b"POST /backend-api/codex/responses HTTP/1.1\r\nHost: 127.0.0.1:6891\r\nContent-Encoding: zstd\r\nContent-Length: 5\r\n\r\n".to_vec();
        request.extend_from_slice(&[0x28, 0xb5, 0x2f, 0xfd, 0xff]);
        let parsed = parse_request_head(&request).expect("binary body is not part of the head");
        assert!(is_codex_path(&parsed.path));
        assert!(request_is_loopback_safe(&request));
        let rewritten = rewrite_request_target(&request, "/v1/responses").unwrap();
        assert!(rewritten.ends_with(&[0x28, 0xb5, 0x2f, 0xfd, 0xff]));

        let invalid = b"POST /v1/responses HTTP/1.1\r\nHost: \xff\r\n\r\n";
        assert!(parse_request_head(invalid).is_none());
    }

    #[test]
    fn loopback_host_parser_handles_ipv6_and_ports() {
        assert!(host_is_loopback("127.0.0.1:6891"));
        assert!(host_is_loopback("[::1]:6891"));
        assert!(!host_is_loopback("127.0.0.2:6891"));
        assert!(!host_is_loopback("example.com:6891"));
    }

    #[test]
    fn health_probe_marker_matches_response_header() {
        assert_eq!(ROUTER_HEALTH_MARKER, ROUTER_HEALTH_HEADER.as_bytes());
    }

    #[test]
    fn public_health_probe_requires_exact_status_and_marker_but_no_token() {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\n{ROUTER_HEALTH_HEADER}\r\n{ROUTER_VERSION_HEADER_NAME}: {ROUTER_RUNTIME_VERSION}\r\nConnection: close\r\n\r\nproxy"
        );
        assert!(router_health_public_matches(response.as_bytes()));

        let wrong_status = response.replacen("200 OK", "503 Service Unavailable", 1);
        assert!(!router_health_public_matches(wrong_status.as_bytes()));
        let wrong_marker = response.replace(ROUTER_HEALTH_HEADER, "X-Headroom-Codex-Router: 0");
        assert!(!router_health_public_matches(wrong_marker.as_bytes()));
        let marker_in_body = response.replace(ROUTER_HEALTH_HEADER, "X-Not-Our-Header: 1");
        assert!(!router_health_public_matches(marker_in_body.as_bytes()));
    }

    #[test]
    fn owned_health_probe_requires_matching_token_and_current_version() {
        let token = "01234567-89ab-cdef-0123-456789abcdef";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\n{ROUTER_HEALTH_HEADER}\r\n{ROUTER_VERSION_HEADER_NAME}: {ROUTER_RUNTIME_VERSION}\r\n{ROUTER_HEALTH_PROBE_ECHO_HEADER_NAME}: {token}\r\nConnection: close\r\n\r\nproxy"
        );
        assert_eq!(
            router_health_owned_version(response.as_bytes(), token).as_deref(),
            Some(ROUTER_RUNTIME_VERSION)
        );
        assert!(probe_response_is_owned_current(response.as_bytes(), token));
        assert!(!probe_response_is_owned_current(
            response.as_bytes(),
            "wrong-token"
        ));

        let old = response.replace(ROUTER_RUNTIME_VERSION, "2/0.0.0");
        assert!(!probe_response_is_owned_current(old.as_bytes(), token));
        let missing_echo = response.replace(
            &format!("{ROUTER_HEALTH_PROBE_ECHO_HEADER_NAME}: {token}\r\n"),
            "",
        );
        assert!(!probe_response_is_owned_current(
            missing_echo.as_bytes(),
            token
        ));
    }

    #[test]
    fn health_probe_rejects_duplicate_sensitive_headers() {
        let token = "01234567-89ab-cdef-0123-456789abcdef";
        let response = format!(
            "HTTP/1.1 200 OK\r\n{ROUTER_HEALTH_HEADER}\r\n{ROUTER_HEALTH_HEADER}\r\n{ROUTER_VERSION_HEADER_NAME}: {ROUTER_RUNTIME_VERSION}\r\n{ROUTER_HEALTH_PROBE_ECHO_HEADER_NAME}: {token}\r\n\r\n"
        );
        assert!(parse_router_health_response(response.as_bytes()).is_none());
    }

    #[test]
    fn router_version_compatibility_ignores_patch_release_but_rejects_other_protocols() {
        assert!(router_version_compatible(ROUTER_RUNTIME_VERSION));
        assert!(router_version_compatible("2/0.0.0"));
        assert!(!router_version_compatible("20/1.0.0"));
        assert!(!router_version_compatible("1/1.0.0"));
        assert!(!router_version_compatible("2"));
        assert!(!router_version_compatible("2evil/1.0.0"));
    }

    #[test]
    fn intercept_identity_requires_status_header_length_and_body() {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{}: {}\r\n\r\n{}",
            INTERCEPT_IDENTITY_BODY.len(),
            INTERCEPT_IDENTITY_HEADER_NAME,
            INTERCEPT_IDENTITY_HEADER_VALUE,
            INTERCEPT_IDENTITY_BODY
        );
        assert!(intercept_identity_matches(response.as_bytes()));
        assert!(!intercept_identity_matches(
            response
                .replace(INTERCEPT_IDENTITY_HEADER_VALUE, "headroom-intercept-v0")
                .as_bytes()
        ));
        assert!(!intercept_identity_matches(
            response
                .replace(INTERCEPT_IDENTITY_BODY, "wrong")
                .as_bytes()
        ));
    }

    #[test]
    fn control_token_comparison_is_length_and_value_sensitive() {
        assert!(constant_time_token_eq("abc", "abc"));
        assert!(!constant_time_token_eq("abc", "abd"));
        assert!(!constant_time_token_eq("abc", "abc-longer"));
    }

    #[test]
    fn control_token_reader_rejects_malformed_values() {
        let path = std::env::temp_dir().join(format!(
            "headroom-codex-router-token-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &path,
            "mode=direct\ncontrol=01234567-89ab-cdef-0123-456789abcdef\n",
        )
        .unwrap();
        assert_eq!(
            read_control_token(&path).as_deref(),
            Some("01234567-89ab-cdef-0123-456789abcdef")
        );
        std::fs::write(&path, "mode=direct\ncontrol=too short\n").unwrap();
        assert!(read_control_token(&path).is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn control_request_requires_post_and_exact_path() {
        let good = format!(
            "POST {ROUTER_CONTROL_PATH} HTTP/1.1\r\nHost: 127.0.0.1:{CODEX_ROUTER_PORT}\r\n\r\n"
        );
        assert!(is_router_control_request(good.as_bytes()));
        let get = good.replacen("POST ", "GET ", 1);
        assert!(!is_router_control_request(get.as_bytes()));
        let suffix = good.replace(
            ROUTER_CONTROL_PATH,
            "/__headroom_codex_router_control/extra",
        );
        assert!(!is_router_control_request(suffix.as_bytes()));
    }
}
