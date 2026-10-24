//! Shared, comment-preserving edits for Codex extension configuration.

use anyhow::{anyhow, Context, Result};
use parking_lot::{ReentrantMutex, ReentrantMutexGuard};
use std::path::{Path, PathBuf};
use toml_edit::DocumentMut;

static CONFIG_EDIT_LOCK: ReentrantMutex<()> = ReentrantMutex::new(());

pub(crate) fn lock_codex_config() -> ReentrantMutexGuard<'static, ()> {
    CONFIG_EDIT_LOCK.lock()
}

pub(crate) fn codex_config_path() -> PathBuf {
    crate::client_adapters::codex_home().join("config.toml")
}

fn target_path(path: &Path) -> Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => path
            .canonicalize()
            .context("Cannot resolve the Codex configuration symlink"),
        Ok(_) => Ok(path.to_owned()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(path.to_owned()),
        Err(err) => Err(err).context("Cannot access the Codex configuration"),
    }
}

fn read_source(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(source) => Ok(Some(source)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).context("Cannot read the Codex configuration"),
    }
}

fn parse_document(path: &Path, source: Option<&str>) -> Result<DocumentMut> {
    match source.unwrap_or_default().parse::<DocumentMut>() {
        Ok(doc) => Ok(doc),
        Err(_) => {
            crate::client_adapters::backup_if_exists(path)?;
            // Parser diagnostics include source lines, which may contain credentials.
            Err(anyhow!("Codex configuration is invalid TOML. The original was backed up and left unchanged."))
        }
    }
}

pub(crate) fn read_codex_config() -> Result<DocumentMut> {
    let _lock = lock_codex_config();
    let path = target_path(&codex_config_path())?;
    let source = read_source(&path)?;
    parse_document(&path, source.as_deref())
}

pub(crate) fn edit_codex_config<T>(edit: impl FnOnce(&mut DocumentMut) -> Result<T>) -> Result<T> {
    edit_config_at(&codex_config_path(), edit)
}

fn edit_config_at<T>(path: &Path, edit: impl FnOnce(&mut DocumentMut) -> Result<T>) -> Result<T> {
    let _lock = lock_codex_config();
    let path = target_path(path)?;
    let source = read_source(&path)?;
    let mut doc = parse_document(&path, source.as_deref())?;
    let result = edit(&mut doc)?;
    let updated = doc.to_string();
    if updated == source.as_deref().unwrap_or_default() {
        return Ok(result);
    }
    if read_source(&path)? != source {
        return Err(anyhow!(
            "Codex configuration changed while editing. Refresh and try again."
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .context("Cannot create the Codex configuration directory")?;
    }
    crate::client_adapters::backup_if_exists(&path)?;
    if read_source(&path)? != source {
        return Err(anyhow!(
            "Codex configuration changed while editing. Refresh and try again."
        ));
    }
    crate::client_adapters::atomic_write(&path, updated.as_bytes())?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_edit_preserves_proxy_markers_comments_and_unknown_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "# >>> headroom-local-community:codex_cli >>>\nmodel_provider = \"headroom\" # keep provider\n# <<< headroom-local-community:codex_cli <<<\n\n[model_providers.headroom]\nbase_url = \"http://127.0.0.1:6891/v1\"\n\n[mcp_servers.example]\ncommand = \"node\"\ncustom_flag = \"keep\" # third-party setting\n";
        std::fs::write(&path, original).unwrap();
        edit_config_at(&path, |doc| {
            doc["mcp_servers"]["example"]["enabled"] = toml_edit::value(false);
            Ok(())
        })
        .unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.starts_with(original));
        assert!(saved.contains("enabled = false"));
        let backup = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.file_name().to_string_lossy().contains("backup-"))
            .unwrap();
        assert_eq!(std::fs::read_to_string(backup.path()).unwrap(), original);
    }

    #[test]
    fn invalid_config_is_backed_up_without_leaking_source_or_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "token = \"do-not-echo-secret";
        std::fs::write(&path, original).unwrap();
        let error = edit_config_at(&path, |_| Ok(())).unwrap_err().to_string();
        assert!(!error.contains("do-not-echo-secret"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn concurrent_external_edit_and_failed_operation_are_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "model = \"original\"\n").unwrap();
        let result = edit_config_at(&path, |doc| {
            doc["model"] = toml_edit::value("ours");
            std::fs::write(&path, "model = \"external\"\n")?;
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "model = \"external\"\n"
        );
        assert!(edit_config_at(&path, |doc| {
            doc["model"] = toml_edit::value("ours");
            Err::<(), _>(anyhow!("cancelled"))
        })
        .is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "model = \"external\"\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_symlink_remains_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let target = dir.path().join("shared.toml");
        std::fs::write(&target, "model = \"original\"\n").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        edit_config_at(&path, |doc| {
            doc["model"] = toml_edit::value("updated");
            Ok(())
        })
        .unwrap();
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(std::fs::read_to_string(&target)
            .unwrap()
            .contains("updated"));
    }
}
