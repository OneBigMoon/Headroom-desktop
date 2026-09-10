//! Compile-time identity and filesystem boundaries for the Community edition.

use std::path::PathBuf;

pub const LOCAL_COMMUNITY: bool = cfg!(feature = "local-community");

pub const ACCOUNT_API_LOOPBACK_SINK: &str = "http://127.0.0.1:9/api/v1";
pub const WORKSPACE_DIR_NAME: &str = ".headroom-local-community";
pub const MCP_SERVER_NAME: &str = "headroom_local_community";
pub const COMMUNITY_ACCOUNT_BILLING_UNAVAILABLE: &str =
    "Headroom Local Community does not provide accounts or billing.";

pub fn workspace_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(WORKSPACE_DIR_NAME)
}

pub fn config_dir() -> PathBuf {
    workspace_dir().join("config")
}

pub fn huggingface_home_dir() -> PathBuf {
    workspace_dir().join("cache").join("huggingface")
}

pub fn huggingface_hub_cache_dir() -> PathBuf {
    huggingface_home_dir().join("hub")
}

pub fn require_account_billing() -> Result<(), String> {
    if LOCAL_COMMUNITY {
        return Err(COMMUNITY_ACCOUNT_BILLING_UNAVAILABLE.to_string());
    }

    Ok(())
}

pub fn apply_runtime_env(command: &mut std::process::Command) -> &mut std::process::Command {
    command
        .env("HEADROOM_WORKSPACE_DIR", workspace_dir())
        .env("HEADROOM_CONFIG_DIR", config_dir())
        .env("HEADROOM_TELEMETRY", "off")
        .env("HEADROOM_BEACON", "0")
        .env("HF_HOME", huggingface_home_dir())
        .env("HF_HUB_CACHE", huggingface_hub_cache_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn command_env(command: &Command, key: &str) -> Option<String> {
        command.get_envs().find_map(|(name, value)| {
            (name == key)
                .then(|| value.and_then(|value| value.to_str()).map(str::to_owned))
                .flatten()
        })
    }

    #[test]
    fn runtime_env_overrides_telemetry_and_uses_community_model_cache() {
        // Other integration tests temporarily redirect HOME to hermetic
        // profiles. Serialize this read of the process-global environment so
        // the expected cache path cannot straddle another test's HOME swap.
        let _home_lock = crate::test_env_lock::lock_home();
        let mut command = crate::proc::command("headroom-ai");
        command
            .env("HEADROOM_TELEMETRY", "on")
            .env("HEADROOM_BEACON", "on")
            .env("HF_HOME", "/shared/huggingface")
            .env("HF_HUB_CACHE", "/shared/huggingface/hub");

        apply_runtime_env(&mut command);

        assert_eq!(
            command_env(&command, "HEADROOM_TELEMETRY").as_deref(),
            Some("off")
        );
        assert_eq!(
            command_env(&command, "HEADROOM_BEACON").as_deref(),
            Some("0")
        );
        assert_eq!(
            command_env(&command, "HF_HOME").as_deref(),
            huggingface_home_dir().to_str()
        );
        assert_eq!(
            command_env(&command, "HF_HUB_CACHE").as_deref(),
            huggingface_hub_cache_dir().to_str()
        );
    }

    #[cfg(feature = "local-community")]
    #[test]
    fn community_rejects_account_and_billing_operations() {
        assert_eq!(
            require_account_billing().expect_err("Community must reject account operations"),
            COMMUNITY_ACCOUNT_BILLING_UNAVAILABLE
        );
    }
}
