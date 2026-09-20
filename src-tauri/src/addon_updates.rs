use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddonUpdateCheck {
    pub id: &'static str,
    pub latest_version: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Copy)]
enum UpdateSource {
    Pypi {
        project: &'static str,
    },
    Npm {
        package: &'static str,
    },
    GithubRelease {
        repository: &'static str,
    },
    GithubPluginManifest {
        repository: &'static str,
        path: &'static str,
    },
    GithubPackageJson {
        repository: &'static str,
    },
}

const MARKITDOWN: UpdateSource = UpdateSource::Pypi {
    project: "markitdown",
};
const HEADROOM: UpdateSource = UpdateSource::Pypi {
    project: "headroom-ai",
};
const SERENA: UpdateSource = UpdateSource::Pypi {
    project: "serena-agent",
};
const CONTEXT7: UpdateSource = UpdateSource::Npm {
    package: "%40upstash%2Fcontext7-mcp",
};
const RTK: UpdateSource = UpdateSource::GithubRelease {
    repository: "rtk-ai/rtk",
};
const CODEBASE_MEMORY: UpdateSource = UpdateSource::GithubRelease {
    repository: "DeusData/codebase-memory-mcp",
};
const PONYTAIL: UpdateSource = UpdateSource::GithubRelease {
    repository: "DietrichGebert/ponytail",
};
const CAVEMAN: UpdateSource = UpdateSource::GithubPluginManifest {
    repository: "JuliusBrussee/caveman",
    path: "plugins/caveman/.codex-plugin/plugin.json",
};
const ALLINLUNA: UpdateSource = UpdateSource::GithubPluginManifest {
    repository: "zenx0x/allinluna",
    path: "plugins/allinluna/.codex-plugin/plugin.json",
};
const OPENSPEC: UpdateSource = UpdateSource::GithubRelease {
    repository: "Fission-AI/OpenSpec",
};
/// Addons whose content is delivered by a marketplace Headroom does not own,
/// so it can neither read their release cadence nor force a refresh.
///
/// `superpowers` and `codex-security` install from Codex's own
/// `openai-curated` marketplace:
/// `codex plugin marketplace upgrade` refuses every source that is not a Git
/// checkout, and Codex syncs that snapshot on its own schedule. Comparing the
/// installed version against obra/superpowers' GitHub release therefore
/// advertised an update that a click could not deliver -- the click
/// reinstalled whatever the curated snapshot currently held, Headroom
/// reported "updated", and the offer came straight back (2026-09-10, the
/// user's report after several rounds of this: installed 5.0.7, offered
/// 6.3.0, unchanged after "Superpowers 已更新。"). Nothing Headroom can do
/// advances that version, so the card reports it without an update action;
/// Codex updates the curated plugin as its snapshot moves.
const HOST_MANAGED_ADDONS: &[&str] = &["superpowers", "codex-security"];

/// Whether Headroom can deliver an update for this addon.
fn tracks_updates(id: &str) -> bool {
    !HOST_MANAGED_ADDONS.contains(&id)
}

/// A host-managed addon still needs an answer so the card stops showing a
/// spinner; `None` with no error is "not tracked here", not "check failed".
fn host_managed_check(id: &'static str) -> AddonUpdateCheck {
    AddonUpdateCheck {
        id,
        latest_version: None,
        error: None,
    }
}
const GSTACK: UpdateSource = UpdateSource::GithubPackageJson {
    repository: "garrytan/gstack",
};
const RALPH_LOOP: UpdateSource = UpdateSource::GithubRelease {
    repository: "SantanderAI/ralph",
};
const STOP_THAT_SHIT: UpdateSource = UpdateSource::GithubRelease {
    repository: "lennney/stop-that-shit",
};
const AGENT_GUARD: UpdateSource = UpdateSource::GithubRelease {
    repository: "JeongJaeSoon/agent-guard",
};
const GRILL_ME: UpdateSource = UpdateSource::GithubPluginManifest {
    repository: "joshuawheelock/grill-me",
    path: ".codex-plugin/plugin.json",
};

fn normalize_version(raw: &str) -> Option<String> {
    let value = raw.trim().trim_start_matches(['v', 'V']);
    let mut parts = value.split('.');
    let major = parts.next()?;
    let minor = parts.next()?;
    if major.is_empty()
        || minor.is_empty()
        || !major.chars().all(|c| c.is_ascii_digit())
        || !minor.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(value.to_string())
}

fn version_from_release_url(path: &str) -> Option<String> {
    normalize_version(path.rsplit('/').next()?)
}

fn json_version(value: &Value, pointer: &str) -> Option<String> {
    normalize_version(value.pointer(pointer)?.as_str()?)
}

fn version_from_plugin_manifest(value: &Value) -> Option<String> {
    json_version(value, "/version")
}

fn github_raw_url(repository: &str, path: &str) -> String {
    format!("https://raw.githubusercontent.com/{repository}/HEAD/{path}")
}

async fn fetch_latest(client: &reqwest::Client, source: UpdateSource) -> anyhow::Result<String> {
    match source {
        UpdateSource::Pypi { project } => {
            let value: Value = client
                .get(format!("https://pypi.org/pypi/{project}/json"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            json_version(&value, "/info/version")
                .ok_or_else(|| anyhow::anyhow!("PyPI response did not contain a valid version"))
        }
        UpdateSource::Npm { package } => {
            let value: Value = client
                .get(format!("https://registry.npmjs.org/{package}/latest"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            json_version(&value, "/version")
                .ok_or_else(|| anyhow::anyhow!("npm response did not contain a valid version"))
        }
        UpdateSource::GithubRelease { repository } => {
            let response = client
                .head(format!("https://github.com/{repository}/releases/latest"))
                .send()
                .await?
                .error_for_status()?;
            version_from_release_url(response.url().path()).ok_or_else(|| {
                anyhow::anyhow!("GitHub latest-release redirect did not contain a valid version")
            })
        }
        UpdateSource::GithubPluginManifest { repository, path } => {
            let value: Value = client
                .get(github_raw_url(repository, path))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            version_from_plugin_manifest(&value)
                .ok_or_else(|| anyhow::anyhow!("GitHub plugin manifest contained no valid version"))
        }
        UpdateSource::GithubPackageJson { repository } => {
            let value: Value = client
                .get(github_raw_url(repository, "package.json"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            json_version(&value, "/version")
                .ok_or_else(|| anyhow::anyhow!("GitHub package.json contained no valid version"))
        }
    }
}

async fn check_one(
    client: &reqwest::Client,
    id: &'static str,
    source: UpdateSource,
) -> AddonUpdateCheck {
    match fetch_latest(client, source).await {
        Ok(latest_version) => AddonUpdateCheck {
            id,
            latest_version: Some(latest_version),
            error: None,
        },
        Err(error) => AddonUpdateCheck {
            id,
            latest_version: None,
            error: Some(error.to_string()),
        },
    }
}

pub async fn check_all() -> Vec<AddonUpdateCheck> {
    let client = match reqwest::Client::builder()
        .user_agent(concat!(
            "headroom-local-community/",
            env!("CARGO_PKG_VERSION")
        ))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(8))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            let message = error.to_string();
            return [
                "headroom",
                "rtk",
                "markitdown",
                "serena",
                "codebase-memory",
                "context7",
                "ponytail",
                "caveman",
                "allinluna",
                "openspec",
                "superpowers",
                "codex-security",
                "gstack",
                "ralph-loop",
                "stop-that-shit",
                "agent-guard",
                "grill-me",
            ]
            .into_iter()
            .map(|id| {
                if tracks_updates(id) {
                    AddonUpdateCheck {
                        id,
                        latest_version: None,
                        error: Some(message.clone()),
                    }
                } else {
                    // A host-managed addon is not checked at all, so a broken
                    // HTTP client is not its problem to report.
                    host_managed_check(id)
                }
            })
            .collect();
        }
    };

    let (
        headroom,
        rtk,
        markitdown,
        serena,
        codebase_memory,
        context7,
        ponytail,
        caveman,
        allinluna,
        openspec,
        superpowers,
        codex_security,
        gstack,
        ralph_loop,
        stop_that_shit,
        agent_guard,
        grill_me,
    ) = tokio::join!(
        check_one(&client, "headroom", HEADROOM),
        check_one(&client, "rtk", RTK),
        check_one(&client, "markitdown", MARKITDOWN),
        check_one(&client, "serena", SERENA),
        check_one(&client, "codebase-memory", CODEBASE_MEMORY),
        check_one(&client, "context7", CONTEXT7),
        check_one(&client, "ponytail", PONYTAIL),
        check_one(&client, "caveman", CAVEMAN),
        check_one(&client, "allinluna", ALLINLUNA),
        check_one(&client, "openspec", OPENSPEC),
        // Not checked: host-managed (see HOST_MANAGED_ADDONS).
        async { host_managed_check("superpowers") },
        async { host_managed_check("codex-security") },
        check_one(&client, "gstack", GSTACK),
        check_one(&client, "ralph-loop", RALPH_LOOP),
        check_one(&client, "stop-that-shit", STOP_THAT_SHIT),
        check_one(&client, "agent-guard", AGENT_GUARD),
        check_one(&client, "grill-me", GRILL_ME),
    );
    vec![
        headroom,
        rtk,
        markitdown,
        serena,
        codebase_memory,
        context7,
        ponytail,
        caveman,
        allinluna,
        openspec,
        superpowers,
        codex_security,
        gstack,
        ralph_loop,
        stop_that_shit,
        agent_guard,
        grill_me,
    ]
}

#[cfg(test)]
mod tests {
    use super::{
        github_raw_url, json_version, normalize_version, tracks_updates,
        version_from_plugin_manifest, version_from_release_url, HOST_MANAGED_ADDONS,
    };
    use serde_json::json;

    /// Codex's curated marketplace is not ours to refresh, so an update offered
    /// for a plugin installed from it can only ever be re-offered: installing
    /// rewrites the same curated snapshot, the version does not move, and the
    /// card asks again (the user's "点了更新还是有提示" loop). Every addon whose
    /// content Headroom installs itself must keep a real check.
    #[test]
    fn host_managed_addons_are_not_advertised_as_updatable() {
        assert!(!tracks_updates("superpowers"));
        assert!(!tracks_updates("codex-security"));
        assert!(HOST_MANAGED_ADDONS.contains(&"superpowers"));
        assert!(HOST_MANAGED_ADDONS.contains(&"codex-security"));
        for id in [
            "gstack",
            "openspec",
            "ponytail",
            "caveman",
            "allinluna",
            "ralph-loop",
            "stop-that-shit",
            "agent-guard",
            "grill-me",
            "rtk",
            "markitdown",
        ] {
            assert!(tracks_updates(id), "{id} must keep its update check");
        }
    }

    #[test]
    fn host_managed_addons_report_no_error_and_no_version() {
        for id in ["superpowers", "codex-security"] {
            let check = super::host_managed_check(id);
            assert_eq!(check.id, id);
            assert!(check.latest_version.is_none());
            assert!(check.error.is_none());
        }
    }

    #[test]
    fn normalizes_registry_and_github_versions() {
        assert_eq!(normalize_version("v0.46.0").as_deref(), Some("0.46.0"));
        assert_eq!(normalize_version(" 4.0.3 ").as_deref(), Some("4.0.3"));
        assert_eq!(normalize_version("latest"), None);
        assert_eq!(
            version_from_release_url("/rtk-ai/rtk/releases/tag/v0.46.0").as_deref(),
            Some("0.46.0")
        );
    }

    #[test]
    fn extracts_versions_from_registry_responses() {
        assert_eq!(
            json_version(&json!({ "info": { "version": "0.1.7" } }), "/info/version").as_deref(),
            Some("0.1.7")
        );
        assert_eq!(
            json_version(&json!({ "version": "4.0.4" }), "/version").as_deref(),
            Some("4.0.4")
        );
    }

    #[test]
    fn extracts_plugin_manifest_version() {
        assert_eq!(
            version_from_plugin_manifest(&json!({ "version": "v2.0.0-rc.7" })).as_deref(),
            Some("2.0.0-rc.7")
        );
        assert_eq!(version_from_plugin_manifest(&json!({})), None);
        assert_eq!(
            version_from_plugin_manifest(&json!({ "version": "latest" })),
            None
        );
    }

    #[test]
    fn github_raw_urls_follow_the_default_branch() {
        assert_eq!(
            github_raw_url("owner/repo", ".claude-plugin/plugin.json"),
            "https://raw.githubusercontent.com/owner/repo/HEAD/.claude-plugin/plugin.json"
        );
    }
}
