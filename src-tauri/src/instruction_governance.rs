//! Audits and refreshes only Headroom-owned instruction blocks.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

static MUTATION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Serialize, Deserialize)]
pub struct Audit {
    pub target: String,
    pub path: String,
    pub baseline_hash: String,
    pub candidate_hash: String,
    pub baseline: String,
    pub candidate: String,
    pub findings: Vec<String>,
    pub blocked: bool,
}

pub(crate) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read(path: &Path) -> Result<String> {
    if path.is_symlink() {
        bail!("Instruction target is a symbolic link; review it manually");
    }
    match fs::read_to_string(path) {
        Ok(value) => Ok(value),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

fn audit(target: &str, path: &Path, templates: &[(String, String)]) -> Result<Audit> {
    let baseline = read(path)?;
    let mut candidate = baseline.clone();
    let mut findings = Vec::new();
    let mut blocked = false;
    for (id, body) in templates {
        let start = format!("# >>> headroom-local-community:{id} >>>");
        let end = format!("# <<< headroom-local-community:{id} <<<");
        let starts: Vec<_> = baseline.match_indices(&start).collect();
        let ends: Vec<_> = baseline.match_indices(&end).collect();
        if starts.is_empty() && ends.is_empty() {
            continue;
        }
        if starts.len() != 1 || ends.len() != 1 || starts[0].0 >= ends[0].0 {
            blocked = true;
            findings.push(format!(
                "{id}: duplicate or malformed ownership markers; manual review required"
            ));
            continue;
        }
        let inner = &baseline[starts[0].0 + start.len()..ends[0].0];
        if inner.contains("# >>> headroom-local-community:")
            || inner.contains("# <<< headroom-local-community:")
        {
            blocked = true;
            findings.push(format!(
                "{id}: overlapping ownership blocks; manual review required"
            ));
            continue;
        }
        let lo = candidate.find(&start).unwrap();
        let hi = candidate.find(&end).unwrap() + end.len();
        let replacement = format!("{start}\n{body}\n{end}");
        if candidate[lo..hi] != replacement {
            findings.push(format!(
                "{id}: managed template differs from current canonical source"
            ));
            candidate.replace_range(lo..hi, &replacement);
        } else {
            findings.push(format!("{id}: current managed template"));
        }
    }
    if baseline.contains("headroom:rtk-instructions") {
        findings.push(
            "Legacy RTK instructions present; compare with managed RTK rules before manual removal"
                .into(),
        );
    }
    if blocked {
        candidate = baseline.clone();
    }
    Ok(Audit {
        target: target.into(),
        path: path.display().to_string(),
        baseline_hash: hash(baseline.as_bytes()),
        candidate_hash: hash(candidate.as_bytes()),
        baseline,
        candidate,
        findings,
        blocked,
    })
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Snapshot {
    target: String,
    path: String,
    baseline_hash: String,
    candidate_hash: String,
    created_at: String,
}

#[derive(Serialize)]
pub struct SnapshotSummary {
    pub id: String,
    pub target: String,
    pub created_at: String,
}

pub(crate) fn audits() -> Result<Vec<Audit>> {
    crate::client_adapters::instruction_targets()
        .iter()
        .map(|(id, path, templates)| audit(id, path, templates))
        .collect()
}

fn snapshot_root() -> PathBuf {
    crate::storage::app_data_dir().join("instruction-snapshots")
}

#[tauri::command]
pub fn audit_instructions() -> Result<Vec<Audit>, String> {
    audits().map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn apply_instruction_candidate(
    target: String,
    baseline_hash: String,
    candidate_hash: String,
) -> Result<String, String> {
    let _guard = MUTATION_LOCK
        .lock()
        .map_err(|_| "Instruction operation lock failed".to_string())?;
    apply(&target, &baseline_hash, &candidate_hash).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn restore_instruction_snapshot(id: String) -> Result<(), String> {
    let _guard = MUTATION_LOCK
        .lock()
        .map_err(|_| "Instruction operation lock failed".to_string())?;
    restore(&id).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn list_instruction_snapshots() -> Result<Vec<SnapshotSummary>, String> {
    let root = snapshot_root();
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let id = entry.file_name().to_string_lossy().to_string();
        if uuid::Uuid::parse_str(&id).is_ok() && entry.path().join("manifest.json").is_file() {
            let bytes = fs::read(entry.path().join("manifest.json")).map_err(|e| e.to_string())?;
            let record: Snapshot = serde_json::from_slice(&bytes)
                .map_err(|e| format!("Invalid snapshot {id}: {e}"))?;
            ids.push(SnapshotSummary {
                id,
                target: record.target,
                created_at: record.created_at,
            });
        }
    }
    ids.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
    Ok(ids)
}

pub(crate) fn apply(target: &str, baseline_hash: &str, candidate_hash: &str) -> Result<String> {
    let report = audits()?
        .into_iter()
        .find(|a| a.target == target)
        .context("Unknown instruction target")?;
    if report.blocked {
        bail!("Malformed managed blocks require review");
    }
    if report.baseline_hash != baseline_hash || report.candidate_hash != candidate_hash {
        bail!("Instructions or templates changed since preview; refresh the audit");
    }
    if report.baseline == report.candidate {
        bail!("No template changes to apply");
    }
    install(&report, &snapshot_root())
}

fn install(report: &Audit, root: &Path) -> Result<String> {
    let path = Path::new(&report.path);
    if hash(read(path)?.as_bytes()) != report.baseline_hash {
        bail!("Instructions changed before apply");
    }
    let id = uuid::Uuid::new_v4().to_string();
    let dir = root.join(&id);
    fs::create_dir_all(root)?;
    fs::create_dir(&dir)?;
    let snapshot = Snapshot {
        target: report.target.clone(),
        path: report.path.clone(),
        baseline_hash: report.baseline_hash.clone(),
        candidate_hash: report.candidate_hash.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    crate::client_adapters::atomic_write(&dir.join("baseline.md"), report.baseline.as_bytes())?;
    crate::client_adapters::atomic_write(&dir.join("candidate.md"), report.candidate.as_bytes())?;
    crate::client_adapters::atomic_write(
        &dir.join("manifest.json"),
        &serde_json::to_vec_pretty(&snapshot)?,
    )?;
    // Check again after writing the backup, before touching the user's file.
    if hash(read(path)?.as_bytes()) != report.baseline_hash {
        bail!("Instructions changed during backup");
    }
    crate::client_adapters::atomic_write(path, report.candidate.as_bytes())?;
    if hash(read(path)?.as_bytes()) != report.candidate_hash {
        bail!("Applied file verification failed; backup retained");
    }
    Ok(id)
}

pub(crate) fn restore(id: &str) -> Result<()> {
    uuid::Uuid::parse_str(id).context("Invalid snapshot identifier")?;
    let dir = snapshot_root().join(id);
    let snapshot: Snapshot = serde_json::from_slice(&fs::read(dir.join("manifest.json"))?)?;
    let targets = crate::client_adapters::instruction_targets();
    if !targets
        .iter()
        .any(|(target, path, _)| target == &snapshot.target && path == Path::new(&snapshot.path))
    {
        bail!("Snapshot no longer matches a configured instruction target");
    }
    restore_snapshot(&dir, &snapshot)
}

fn restore_snapshot(dir: &Path, snapshot: &Snapshot) -> Result<()> {
    let baseline = fs::read(dir.join("baseline.md"))?;
    if hash(&baseline) != snapshot.baseline_hash {
        bail!("Backup checksum mismatch");
    }
    let path = Path::new(&snapshot.path);
    let current = hash(read(path)?.as_bytes());
    if current == snapshot.baseline_hash {
        return Ok(());
    }
    if current != snapshot.candidate_hash {
        bail!("Instructions changed after apply; refusing to overwrite newer edits");
    }
    crate::client_adapters::atomic_write(path, &baseline)?;
    if hash(read(path)?.as_bytes()) != snapshot.baseline_hash {
        bail!("Restore checksum mismatch");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_files_remain_missing_and_backup_failure_preserves_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        let templates = [("rtk".into(), "new".into())];
        let empty = audit("test", &path, &templates).unwrap();
        assert_eq!(empty.baseline, empty.candidate);
        assert!(!path.exists());
        let original = "# >>> headroom-local-community:rtk >>>\nold\n# <<< headroom-local-community:rtk <<<\n";
        fs::write(&path, original).unwrap();
        let report = audit("test", &path, &templates).unwrap();
        let invalid_root = dir.path().join("not-a-directory");
        fs::write(&invalid_root, "keep").unwrap();
        assert!(install(&report, &invalid_root).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert_eq!(fs::read_to_string(invalid_root).unwrap(), "keep");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_targets_are_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let actual = dir.path().join("actual.md");
        let link = dir.path().join("AGENTS.md");
        fs::write(&actual, "user rules").unwrap();
        std::os::unix::fs::symlink(&actual, &link).unwrap();
        assert!(audit("test", &link, &[]).is_err());
        assert_eq!(fs::read_to_string(actual).unwrap(), "user rules");
        assert!(link.is_symlink());
    }
    #[test]
    fn nested_or_crossed_blocks_are_rejected_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        for tail in [
            "# <<< headroom-local-community:serena <<<\n# <<< headroom-local-community:rtk <<<\n",
            "# <<< headroom-local-community:rtk <<<\n# <<< headroom-local-community:serena <<<\n",
        ] {
            let original = format!("# >>> headroom-local-community:rtk >>>\n# >>> headroom-local-community:serena >>>\n{tail}");
            fs::write(&path, &original).unwrap();
            let report = audit(
                "test",
                &path,
                &[
                    ("rtk".into(), "new".into()),
                    ("serena".into(), "new".into()),
                ],
            )
            .unwrap();
            assert!(report.blocked);
            assert_eq!(report.candidate, original);
        }
    }
    #[test]
    fn audit_refresh_preserves_user_bytes_and_does_not_enable_missing_tools() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        let original = "User rules\n\n# >>> headroom-local-community:rtk >>>\nold\n# <<< headroom-local-community:rtk <<<\nTail  \n";
        fs::write(&path, original).unwrap();
        let report = audit(
            "test",
            &path,
            &[
                ("rtk".into(), "new".into()),
                ("serena".into(), "absent".into()),
            ],
        )
        .unwrap();
        assert_eq!(report.candidate, original.replace("\nold\n", "\nnew\n"));
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        let id = install(&report, &dir.path().join("backups")).unwrap();
        let backup = dir.path().join("backups").join(id);
        let snapshot =
            serde_json::from_slice(&fs::read(backup.join("manifest.json")).unwrap()).unwrap();
        restore_snapshot(&backup, &snapshot).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }
    #[test]
    fn malformed_markers_block_all_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        fs::write(&path, "# >>> headroom-local-community:rtk >>>\nmissing end").unwrap();
        let report = audit("test", &path, &[("rtk".into(), "new".into())]).unwrap();
        assert!(report.blocked);
        assert_eq!(report.baseline, report.candidate);
    }
    #[test]
    fn changed_file_and_corrupt_backup_are_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        let original =
            "# >>> headroom-local-community:rtk >>>\nold\n# <<< headroom-local-community:rtk <<<\n";
        fs::write(&path, original).unwrap();
        let report = audit("test", &path, &[("rtk".into(), "new".into())]).unwrap();
        fs::write(&path, "newer edits").unwrap();
        assert!(install(&report, dir.path()).is_err());
        fs::write(&path, original).unwrap();
        let id = install(&report, dir.path()).unwrap();
        let backup = dir.path().join(id);
        let snapshot =
            serde_json::from_slice(&fs::read(backup.join("manifest.json")).unwrap()).unwrap();
        fs::write(&path, "newer edits").unwrap();
        assert!(restore_snapshot(&backup, &snapshot).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "newer edits");
        fs::write(&path, &report.candidate).unwrap();
        fs::write(backup.join("baseline.md"), "corrupt").unwrap();
        assert!(restore_snapshot(&backup, &snapshot).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), report.candidate);
    }
}
