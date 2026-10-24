//! Read-only skill discovery and explicit, bounded imports for Codex.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use toml_edit::{DocumentMut, Item, Table, Value};

const MANIFEST: &str = "SKILL.md";
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
const MAX_SCAN_ENTRIES: usize = 10_000;
const MAX_SKILLS: usize = 500;
const MAX_SCAN_DEPTH: usize = 8;
const MAX_IMPORT_FILES: usize = 2048;
const MAX_IMPORT_BYTES: u64 = 20 * 1024 * 1024;
const MAX_REPOSITORY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_GIT_TREE_BYTES: u64 = 4 * 1024 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub skill_path: String,
    pub directory: String,
    pub scope: String,
    pub enabled: bool,
    pub can_toggle: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCatalog {
    pub skills: Vec<SkillSummary>,
    pub warnings: Vec<String>,
    pub config_path: String,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillScan {
    pub skills: Vec<SkillSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSkillSummary {
    pub name: String,
    pub description: String,
    pub subdirectory: String,
    pub install_directory: String,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSkillScan {
    pub skills: Vec<GitSkillSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDetails {
    pub skill_path: String,
    pub content: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillTarget {
    Global,
    Project,
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|_| "技能操作未完成，请重试。".to_string())?
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
pub async fn list_skills(project_path: Option<String>) -> Result<SkillCatalog, String> {
    blocking(move || {
        let home = dirs::home_dir().context("无法确定用户目录。")?;
        let mut roots = vec![
            (home.join(".agents/skills"), "global_agents"),
            (
                crate::client_adapters::codex_home().join("skills"),
                "global_codex",
            ),
            (PathBuf::from("/etc/codex/skills"), "admin"),
        ];
        if let Some(project) = project_path.filter(|path| !path.trim().is_empty()) {
            let project = existing_directory(Path::new(&project))?;
            roots.push((project.join(".agents/skills"), "project_agents"));
            roots.push((project.join(".codex/skills"), "project_codex"));
        }
        let config = crate::extension_config::read_codex_config()?;
        let scan = scan_roots(&roots, &config, true)?;
        Ok(SkillCatalog {
            skills: scan.skills,
            warnings: scan.warnings,
            config_path: display_path(&crate::extension_config::codex_config_path()),
        })
    })
    .await
}

#[tauri::command]
pub async fn scan_skill_source(source_path: String) -> Result<SkillScan, String> {
    blocking(move || {
        let source = existing_directory(Path::new(&source_path))?;
        scan_roots(&[(source, "local")], &DocumentMut::new(), false)
    })
    .await
}

#[tauri::command]
pub async fn read_skill(skill_path: String) -> Result<SkillDetails, String> {
    blocking(move || {
        let path = manifest_path(Path::new(&skill_path))?;
        Ok(SkillDetails {
            skill_path: display_path(&path),
            content: read_manifest(&path)?,
        })
    })
    .await
}

#[tauri::command]
pub async fn set_skill_enabled(skill_path: String, enabled: bool) -> Result<SkillSummary, String> {
    blocking(move || {
        let path = manifest_path(Path::new(&skill_path))?;
        let mut summary = summary_at(&path, inferred_scope(&path), true)?;
        crate::extension_config::edit_codex_config(|doc| set_config_enabled(doc, &path, enabled))?;
        summary.enabled = enabled;
        Ok(summary)
    })
    .await
}

#[tauri::command]
pub async fn import_skill(
    source_path: String,
    target: SkillTarget,
    project_path: Option<String>,
) -> Result<SkillSummary, String> {
    blocking(move || {
        let source = existing_directory(Path::new(&source_path))?;
        let name = source
            .file_name()
            .context("请选择一个具体的技能目录。")?
            .to_os_string();
        let config = crate::extension_config::read_codex_config()?;
        let (root, scope) = import_root(target, project_path.as_deref())?;
        import_local_at(&source, &root, &name, scope, &config)
    })
    .await
}

#[tauri::command]
pub async fn scan_git_skills(repository_url: String) -> Result<GitSkillScan, String> {
    blocking(move || {
        let url = public_repository_url(&repository_url)?;
        let (import_root, _) = resolve_import_root(SkillTarget::Global, None)?;
        let temporary = tempfile::tempdir().context("无法创建仓库临时目录。")?;
        let checkout = clone_repository(&url, temporary.path())?;
        let mut listing = git_command();
        listing
            .arg("-C")
            .arg(&checkout)
            .args(["ls-tree", "-r", "-z", "HEAD"]);
        let bytes = git_output(listing, temporary.path(), MAX_GIT_TREE_BYTES)?;
        let (manifests, warnings) = git_skill_manifests(&bytes)?;
        let mut scan = GitSkillScan {
            skills: Vec::new(),
            warnings,
        };
        for manifest in manifests {
            let mut show = git_command();
            show.arg("-C")
                .arg(&checkout)
                .args(["cat-file", "blob", &manifest.object_id]);
            let result =
                git_output(show, temporary.path(), MAX_MANIFEST_BYTES).and_then(|content| {
                    git_skill_summary(&manifest.subdirectory, &content, &url, &import_root)
                });
            match result {
                Ok(skill) => scan.skills.push(skill),
                Err(error) => scan
                    .warnings
                    .push(format!("{}：{error}", manifest.subdirectory)),
            }
        }
        Ok(scan)
    })
    .await
}

#[tauri::command]
pub async fn import_git_skill(
    repository_url: String,
    subdirectory: String,
    target: SkillTarget,
    project_path: Option<String>,
) -> Result<SkillSummary, String> {
    blocking(move || {
        let url = public_repository_url(&repository_url)?;
        let relative = safe_subdirectory(&subdirectory)?;
        let config = crate::extension_config::read_codex_config()?;
        let temporary = tempfile::tempdir().context("无法创建仓库临时目录。")?;
        let checkout = clone_repository(&url, temporary.path())?;
        let (root, scope) = import_root(target, project_path.as_deref())?;
        let stage = new_stage(&root)?;
        let revision = if relative.as_os_str().is_empty() {
            "HEAD".to_string()
        } else {
            format!("HEAD:{}", relative.to_string_lossy().replace('\\', "/"))
        };
        let mut archive = git_command();
        archive
            .arg("-C")
            .arg(&checkout)
            .args(["archive", "--format=tar", &revision]);
        archive.stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = archive.spawn().context("无法读取仓库中的技能目录。")?;
        let stdout = child.stdout.take().context("无法读取技能归档。")?;
        let extracted = unpack_skill_archive(stdout, stage.path());
        if extracted.is_err() {
            terminate_git(&mut child);
        }
        let status = child.wait().context("无法完成技能归档读取。")?;
        extracted?;
        anyhow::ensure!(
            status.success(),
            "仓库或所选目录不可用；仅支持无需登录的公开 HTTPS Git 仓库。"
        );
        let name = git_import_name(&url, &relative);
        publish_stage(stage, &root, &name, scope, &config)
    })
    .await
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn existing_directory(path: &Path) -> Result<PathBuf> {
    let path = path.canonicalize().context("目录不存在或无法访问。")?;
    anyhow::ensure!(path.is_dir(), "请选择目录，而不是文件。");
    Ok(path)
}

fn manifest_path(path: &Path) -> Result<PathBuf> {
    let path = if path.is_dir() {
        path.join(MANIFEST)
    } else {
        path.to_path_buf()
    };
    anyhow::ensure!(
        path.file_name().is_some_and(|name| name == MANIFEST),
        "只允许读取 SKILL.md。"
    );
    // Directory symlinks are discoverable; a manifest symlink must not disclose an unrelated file.
    let metadata = fs::symlink_metadata(&path).context("无法访问 SKILL.md。")?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "SKILL.md 必须是普通文件，不能是符号链接。"
    );
    path.canonicalize().context("无法解析技能路径。")
}

fn read_manifest(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path).context("无法读取技能文件。")?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "SKILL.md 必须是普通文件。"
    );
    anyhow::ensure!(
        metadata.len() <= MAX_MANIFEST_BYTES,
        "SKILL.md 超过 256 KiB 上限。"
    );
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_MANIFEST_BYTES,
        "SKILL.md 超过 256 KiB 上限。"
    );
    String::from_utf8(bytes).context("SKILL.md 必须使用 UTF-8 编码。")
}

fn metadata(content: &str, fallback: &str) -> (String, String) {
    let lines: Vec<_> = content.trim_start_matches('\u{feff}').lines().collect();
    let mut name = fallback.to_string();
    let mut description = String::new();
    if lines.first().is_none_or(|line| line.trim() != "---") {
        return (name, description);
    }
    let end = lines
        .iter()
        .skip(1)
        .position(|line| line.trim() == "---")
        .map(|index| index + 1)
        .unwrap_or(1);
    let mut index = 1;
    while index < end {
        let line = lines[index];
        if line.starts_with(char::is_whitespace) {
            index += 1;
            continue;
        }
        if let Some((key, raw)) = line.split_once(':') {
            if matches!(key.trim(), "name" | "description") {
                let raw = raw.trim();
                let value = if matches!(raw, ">" | ">-" | ">+" | "|" | "|-" | "|+") {
                    let mut parts = Vec::new();
                    while index + 1 < end
                        && (lines[index + 1].starts_with(char::is_whitespace)
                            || lines[index + 1].is_empty())
                    {
                        index += 1;
                        parts.push(lines[index].trim());
                    }
                    parts.join(if raw.starts_with('|') { "\n" } else { " " })
                } else if raw.starts_with('"') {
                    serde_json::from_str::<String>(raw)
                        .unwrap_or_else(|_| raw.trim_matches('"').to_string())
                } else if raw.starts_with('\'') && raw.ends_with('\'') {
                    raw.trim_matches('\'').replace("''", "'")
                } else {
                    raw.to_string()
                };
                if key.trim() == "name" && !value.is_empty() {
                    name = value.chars().take(128).collect();
                }
                if key.trim() == "description" {
                    description = value.chars().take(4000).collect();
                }
            }
        }
        index += 1;
    }
    (name, description)
}

fn validate_import_manifest(content: &str) -> Result<()> {
    let mut lines = content.trim_start_matches('\u{feff}').lines();
    anyhow::ensure!(
        lines.next().is_some_and(|line| line.trim() == "---")
            && lines.any(|line| line.trim() == "---"),
        "技能导入需要以 --- 包围并闭合的 YAML frontmatter。"
    );
    let (name, description) = metadata(content, "");
    anyhow::ensure!(
        !name.trim().is_empty() && !description.trim().is_empty(),
        "技能导入需要非空的 name 和 description；原技能和已有文件未修改。"
    );
    Ok(())
}

fn summary_at(path: &Path, scope: &str, can_toggle: bool) -> Result<SkillSummary> {
    let content = read_manifest(path)?;
    let directory = path.parent().context("技能目录无效。")?;
    let fallback = directory.file_name().unwrap_or_default().to_string_lossy();
    let (name, description) = metadata(&content, &fallback);
    Ok(SkillSummary {
        name,
        description,
        skill_path: display_path(path),
        directory: display_path(directory),
        scope: scope.into(),
        enabled: true,
        can_toggle,
    })
}

fn inferred_scope(path: &Path) -> &'static str {
    if path
        .components()
        .any(|component| component.as_os_str() == ".system")
    {
        return "builtin";
    }
    if path.starts_with("/etc/codex/skills") {
        return "admin";
    }
    if dirs::home_dir().is_some_and(|home| path.starts_with(home.join(".agents/skills"))) {
        return "global_agents";
    }
    if path.starts_with(crate::client_adapters::codex_home().join("skills")) {
        return "global_codex";
    }
    let parts: Vec<_> = path.components().collect();
    for pair in parts.windows(2) {
        if pair[1].as_os_str() == "skills" {
            if pair[0].as_os_str() == ".agents" {
                return "project_agents";
            }
            if pair[0].as_os_str() == ".codex" {
                return "project_codex";
            }
        }
    }
    "local"
}

fn config_overrides(doc: &DocumentMut) -> Result<HashMap<PathBuf, bool>> {
    let mut overrides = HashMap::new();
    let Some(config) = doc.get("skills").and_then(|skills| skills.get("config")) else {
        return Ok(overrides);
    };
    let mut add = |path: Option<&str>, enabled: Option<&Value>| -> Result<()> {
        if let Some(path) = path {
            let enabled = enabled
                .map(|value| {
                    value
                        .as_bool()
                        .context("skills.config.enabled 必须是布尔值。")
                })
                .transpose()?
                .unwrap_or(true);
            let path = PathBuf::from(path);
            overrides.insert(path.canonicalize().unwrap_or(path), enabled);
        }
        Ok(())
    };
    if let Some(tables) = config.as_array_of_tables() {
        for table in tables {
            add(
                table.get("path").and_then(Item::as_str),
                table.get("enabled").and_then(Item::as_value),
            )?;
        }
    } else if let Some(array) = config.as_array() {
        for entry in array {
            let table = entry
                .as_inline_table()
                .context("skills.config 必须是技能配置表数组。");
            let table = table?;
            add(
                table.get("path").and_then(Value::as_str),
                table.get("enabled"),
            )?;
        }
    } else {
        bail!("skills.config 必须是技能配置表数组。");
    }
    Ok(overrides)
}

fn flag_value(previous: Option<&Value>, enabled: bool) -> Value {
    let mut value = Value::from(enabled);
    if let Some(previous) = previous {
        *value.decor_mut() = previous.decor().clone();
    }
    value
}

fn matching_path(value: Option<&str>, path: &Path) -> bool {
    value.is_some_and(|raw| {
        let raw = PathBuf::from(raw);
        raw.canonicalize().unwrap_or(raw) == path
    })
}

fn set_config_enabled(doc: &mut DocumentMut, path: &Path, enabled: bool) -> Result<()> {
    config_overrides(doc)?;
    if !doc.contains_key("skills") {
        doc.insert("skills", Item::Table(Table::new()));
    }
    let skills = doc
        .get_mut("skills")
        .and_then(Item::as_table_like_mut)
        .context("skills 必须是配置表；原配置未修改。")?;
    if !skills.contains_key("config") {
        skills.insert(
            "config",
            Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
        );
    }
    let config = skills.get_mut("config").context("无法更新技能配置。")?;
    if let Some(tables) = config.as_array_of_tables_mut() {
        let mut matched = false;
        for table in tables.iter_mut() {
            if matching_path(table.get("path").and_then(Item::as_str), path) {
                let value = flag_value(table.get("enabled").and_then(Item::as_value), enabled);
                table.insert("enabled", Item::Value(value));
                matched = true;
            }
        }
        if !matched {
            let mut table = Table::new();
            table.insert("path", toml_edit::value(display_path(path)));
            table.insert("enabled", toml_edit::value(enabled));
            tables.push(table);
        }
    } else if let Some(array) = config.as_array_mut() {
        let mut matched = false;
        for entry in array.iter_mut() {
            let table = entry
                .as_inline_table_mut()
                .context("skills.config 必须是技能配置表数组。")?;
            if matching_path(table.get("path").and_then(Value::as_str), path) {
                let value = flag_value(table.get("enabled"), enabled);
                table.insert("enabled", value);
                matched = true;
            }
        }
        if !matched {
            let mut table = toml_edit::InlineTable::new();
            table.insert("path", Value::from(display_path(path)));
            table.insert("enabled", Value::from(enabled));
            array.push(Value::InlineTable(table));
        }
    } else {
        bail!("skills.config 必须是技能配置表数组。");
    }
    Ok(())
}

struct Scanner<'a> {
    result: SkillScan,
    visited: HashSet<PathBuf>,
    entries: usize,
    overrides: &'a HashMap<PathBuf, bool>,
    can_toggle: bool,
}

impl Scanner<'_> {
    fn warn(&mut self, warning: String) {
        if self.result.warnings.len() < 32 {
            self.result.warnings.push(warning);
        }
    }

    fn visit(&mut self, directory: &Path, scope: &str, depth: usize) {
        if self.entries >= MAX_SCAN_ENTRIES || self.result.skills.len() >= MAX_SKILLS {
            return;
        }
        let canonical = match directory.canonicalize() {
            Ok(path) => path,
            Err(_) => return,
        };
        if !canonical.is_dir() || !self.visited.insert(canonical) {
            return;
        }
        let manifest = directory.join(MANIFEST);
        if fs::symlink_metadata(&manifest).is_ok() {
            let skill =
                manifest_path(&manifest).and_then(|path| summary_at(&path, scope, self.can_toggle));
            match skill {
                Ok(mut skill) => {
                    if let Err(error) = read_manifest(Path::new(&skill.skill_path))
                        .and_then(|content| validate_import_manifest(&content))
                    {
                        self.warn(format!("{}：{error}；仍只读显示此技能。", skill.skill_path));
                    }
                    skill.enabled = self
                        .overrides
                        .get(Path::new(&skill.skill_path))
                        .copied()
                        .unwrap_or(true);
                    self.result.skills.push(skill);
                }
                Err(error) => self.warn(format!("{}：{error:#}", display_path(&manifest))),
            }
            return;
        }
        if depth >= MAX_SCAN_DEPTH {
            self.warn(format!("扫描深度达到上限：{}", display_path(directory)));
            return;
        }
        let children = match fs::read_dir(directory) {
            Ok(children) => children,
            Err(_) => {
                self.warn(format!("目录无法读取：{}", display_path(directory)));
                return;
            }
        };
        let mut children: Vec<_> = children
            .take(MAX_SCAN_ENTRIES - self.entries)
            .filter_map(Result::ok)
            .collect();
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            self.entries += 1;
            if self.entries >= MAX_SCAN_ENTRIES || self.result.skills.len() >= MAX_SKILLS {
                self.warn("技能扫描达到数量上限，结果已截断。".into());
                break;
            }
            let name = child.file_name();
            if scope == "global_codex" && depth == 0 && name == ".system" {
                continue;
            }
            if name.to_string_lossy().starts_with(".codexbox-skill-") {
                continue;
            }
            if matches!(
                name.to_str(),
                Some(".git" | "node_modules" | "target" | ".venv" | "venv" | "__pycache__")
            ) {
                continue;
            }
            if child.path().is_dir() {
                self.visit(&child.path(), scope, depth + 1);
            }
        }
    }
}

fn scan_roots(
    roots: &[(PathBuf, &str)],
    config: &DocumentMut,
    can_toggle: bool,
) -> Result<SkillScan> {
    let overrides = config_overrides(config)?;
    let mut scanner = Scanner {
        result: SkillScan::default(),
        visited: HashSet::new(),
        entries: 0,
        overrides: &overrides,
        can_toggle,
    };
    for (root, scope) in roots {
        scanner.visit(root, scope, 0);
    }
    scanner
        .result
        .skills
        .sort_by(|a, b| a.name.cmp(&b.name).then(a.skill_path.cmp(&b.skill_path)));
    Ok(scanner.result)
}

fn import_root(target: SkillTarget, project: Option<&str>) -> Result<(PathBuf, &'static str)> {
    let (root, scope) = resolve_import_root(target, project)?;
    for directory in [root.parent().context("技能导入目录无效。")?, root.as_path()] {
        if !directory.exists() {
            fs::create_dir(directory)?;
        }
    }
    Ok((root.canonicalize()?, scope))
}

fn resolve_import_root(
    target: SkillTarget,
    project: Option<&str>,
) -> Result<(PathBuf, &'static str)> {
    let (base, scope) = match target {
        SkillTarget::Global => (
            existing_directory(&dirs::home_dir().context("无法确定用户目录。")?)?,
            "global_agents",
        ),
        SkillTarget::Project => (
            existing_directory(Path::new(project.context("请选择项目目录。")?))?,
            "project_agents",
        ),
    };
    let root = base.join(".agents/skills");
    for directory in [base.join(".agents"), root.clone()] {
        match fs::symlink_metadata(&directory) {
            Ok(metadata) => anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "导入目标必须是实体目录，不能是符号链接。"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("无法访问技能导入目录。"),
        }
    }
    Ok((root, scope))
}

fn new_stage(root: &Path) -> Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix(".codexbox-skill-")
        .tempdir_in(root)
        .context("无法创建同卷技能暂存目录。")
}

#[derive(Default)]
struct CopyBudget {
    files: usize,
    bytes: u64,
}

impl CopyBudget {
    fn add(&mut self, bytes: u64) -> Result<()> {
        self.files += 1;
        self.bytes = self.bytes.checked_add(bytes).context("技能大小无效。")?;
        anyhow::ensure!(
            self.files <= MAX_IMPORT_FILES && self.bytes <= MAX_IMPORT_BYTES,
            "技能超过导入上限（2048 个文件、20 MiB）。"
        );
        Ok(())
    }
}

fn copy_skill_directory(
    source: &Path,
    destination: &Path,
    budget: &mut CopyBudget,
    depth: usize,
) -> Result<()> {
    anyhow::ensure!(depth <= 32, "技能目录嵌套过深。");
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        anyhow::ensure!(
            !metadata.file_type().is_symlink(),
            "技能包含符号链接；首版导入只接受实际文件和目录，不跟随链接。"
        );
        let target = destination.join(entry.file_name());
        if metadata.is_dir() {
            budget.add(0)?;
            fs::create_dir(&target)?;
            copy_skill_directory(&entry.path(), &target, budget, depth + 1)?;
        } else if metadata.is_file() {
            budget.add(metadata.len())?;
            let mut options = fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW);
            }
            let input = options.open(entry.path())?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            let written = std::io::copy(&mut input.take(metadata.len() + 1), &mut output)?;
            anyhow::ensure!(
                written == metadata.len(),
                "源文件在复制过程中发生变化，请重试。"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &target,
                    fs::Permissions::from_mode(metadata.permissions().mode() & 0o777),
                )?;
            }
        } else {
            bail!("技能只允许普通文件和目录。");
        }
    }
    Ok(())
}

fn import_local_at(
    source: &Path,
    root: &Path,
    name: &std::ffi::OsStr,
    scope: &str,
    config: &DocumentMut,
) -> Result<SkillSummary> {
    anyhow::ensure!(
        !root.starts_with(source),
        "源技能目录不能包含导入目标目录。"
    );
    validate_import_manifest(&read_manifest(&manifest_path(source)?)?)?;
    let stage = new_stage(root)?;
    copy_skill_directory(source, stage.path(), &mut CopyBudget::default(), 0)?;
    publish_stage(stage, root, name, scope, config)
}

fn publish_stage(
    stage: tempfile::TempDir,
    root: &Path,
    name: &std::ffi::OsStr,
    scope: &str,
    config: &DocumentMut,
) -> Result<SkillSummary> {
    let destination = import_destination(root, name)?;
    anyhow::ensure!(
        fs::symlink_metadata(&destination)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
        "目标技能已存在；不会覆盖，请保留现有目录或另选技能。"
    );
    let content = read_manifest(&manifest_path(stage.path())?)?;
    validate_import_manifest(&content)?;
    let mut summary = summary_at(&stage.path().join(MANIFEST), scope, true)?;
    summary.name = metadata(&content, "").0;
    let overrides = config_overrides(config)?;
    let skill_path = destination.join(MANIFEST);
    summary.enabled = overrides.get(&skill_path).copied().unwrap_or(true);
    rename_without_replace(stage.path(), &destination)
        .context("无法发布技能；目标可能已经存在，已有文件未覆盖。")?;
    summary.skill_path = display_path(&skill_path);
    summary.directory = display_path(&destination);
    Ok(summary)
}

fn import_destination(root: &Path, name: &std::ffi::OsStr) -> Result<PathBuf> {
    let name_path = Path::new(name);
    anyhow::ensure!(
        name_path.components().count() == 1
            && matches!(name_path.components().next(), Some(Component::Normal(_)))
            && !name.to_string_lossy().starts_with('.'),
        "技能目录名无效。"
    );
    Ok(root.join(name))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rename_without_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
    #[cfg(target_os = "macos")]
    let result =
        unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn rename_without_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn rename_without_replace(_: &Path, _: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "此平台暂不支持不覆盖的目录导入。",
    ))
}

fn public_repository_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw.trim()).context("请输入公开 HTTPS Git 仓库 URL。")?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "仓库必须使用不含凭据、查询参数或片段的公开 HTTPS URL。"
    );
    Ok(url)
}

fn safe_subdirectory(raw: &str) -> Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() || raw == "." {
        return Ok(PathBuf::new());
    }
    let path = Path::new(raw);
    anyhow::ensure!(
        !raw.contains('\\')
            && path
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
        "仓库子目录必须是安全的相对路径，不能包含 .. 或绝对路径。"
    );
    Ok(path.to_path_buf())
}

fn clone_repository(url: &reqwest::Url, temporary: &Path) -> Result<PathBuf> {
    let checkout = temporary.join("repository");
    let mut clone = git_command();
    clone
        .args([
            "clone",
            "--depth=1",
            "--single-branch",
            "--no-tags",
            "--no-checkout",
            "--",
        ])
        .arg(url.as_str())
        .arg(&checkout);
    run_git(clone, temporary)?;
    Ok(checkout)
}

#[derive(Debug)]
struct GitManifest {
    object_id: String,
    subdirectory: String,
}

fn git_skill_manifests(bytes: &[u8]) -> Result<(Vec<GitManifest>, Vec<String>)> {
    let mut manifests = Vec::new();
    let mut warnings = Vec::new();
    for (index, entry) in bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .enumerate()
    {
        if index >= MAX_SCAN_ENTRIES {
            warnings.push(format!(
                "仓库扫描达到 {MAX_SCAN_ENTRIES} 个文件上限，结果可能不完整。"
            ));
            break;
        }
        let separator = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .context("Git 文件清单无效。")?;
        let header = std::str::from_utf8(&entry[..separator]).context("Git 文件清单无效。")?;
        let fields: Vec<_> = header.split_whitespace().collect();
        anyhow::ensure!(fields.len() == 3, "Git 文件清单无效。");
        let path = match std::str::from_utf8(&entry[separator + 1..]) {
            Ok(path) => path,
            Err(_) => {
                warnings.push("已跳过非 UTF-8 仓库路径。".to_string());
                continue;
            }
        };
        if Path::new(path).file_name().and_then(|name| name.to_str()) != Some(MANIFEST) {
            continue;
        }
        if !matches!(fields[0], "100644" | "100755") || fields[1] != "blob" {
            warnings.push("已跳过非普通文件的 SKILL.md。".to_string());
            continue;
        }
        let relative = match safe_subdirectory(path) {
            Ok(relative) if path.trim() == path => relative,
            _ => {
                warnings.push("已跳过不安全的 SKILL.md 路径。".to_string());
                continue;
            }
        };
        let directory = relative.parent().unwrap_or_else(|| Path::new(""));
        let subdirectory = if directory.as_os_str().is_empty() {
            ".".to_string()
        } else {
            directory.to_string_lossy().into_owned()
        };
        if subdirectory.trim() != subdirectory {
            warnings.push("已跳过含首尾空白的技能目录。".to_string());
            continue;
        }
        anyhow::ensure!(
            matches!(fields[2].len(), 40 | 64)
                && fields[2].bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Git 文件清单无效。"
        );
        if manifests.len() >= MAX_SKILLS {
            warnings.push(format!(
                "仓库扫描达到 {MAX_SKILLS} 个技能上限，结果可能不完整。"
            ));
            break;
        }
        manifests.push(GitManifest {
            object_id: fields[2].to_string(),
            subdirectory,
        });
    }
    Ok((manifests, warnings))
}

fn git_import_name(url: &reqwest::Url, relative: &Path) -> std::ffi::OsString {
    relative
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| {
            std::ffi::OsString::from(
                url.path_segments()
                    .and_then(|mut parts| parts.rfind(|part| !part.is_empty()))
                    .unwrap_or("skill")
                    .trim_end_matches(".git"),
            )
        })
}

fn git_skill_summary(
    subdirectory: &str,
    bytes: &[u8],
    url: &reqwest::Url,
    root: &Path,
) -> Result<GitSkillSummary> {
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_MANIFEST_BYTES,
        "SKILL.md 超过 256 KiB 上限。"
    );
    let content = std::str::from_utf8(bytes).context("SKILL.md 必须使用 UTF-8 编码。")?;
    validate_import_manifest(content)?;
    let (name, description) = metadata(content, "");
    let relative = safe_subdirectory(subdirectory)?;
    let directory = import_destination(root, &git_import_name(url, &relative))?;
    Ok(GitSkillSummary {
        name,
        description,
        subdirectory: subdirectory.to_string(),
        install_directory: display_path(&directory),
    })
}

fn git_command() -> Command {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "credential.helper=",
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.https.allow=always",
            "-c",
            "http.followRedirects=false",
        ])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_ASKPASS")
        .env_remove("SSH_ASKPASS")
        .stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

fn terminate_git(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
}

fn repository_size(root: &Path, total: &mut u64, entries: &mut usize) -> Result<()> {
    let children = match fs::read_dir(root) {
        Ok(children) => children,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in children {
        *entries += 1;
        anyhow::ensure!(*entries <= 50_000, "仓库文件过多。");
        let entry = entry?;
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        *total = total.saturating_add(metadata.len());
        anyhow::ensure!(
            *total <= MAX_REPOSITORY_BYTES,
            "仓库下载超过 128 MiB 上限。"
        );
        if metadata.is_dir() {
            repository_size(&entry.path(), total, entries)?;
        }
    }
    Ok(())
}

fn run_git(mut command: Command, root: &Path) -> Result<()> {
    command.stdout(Stdio::null());
    run_git_checked(command, root, None)
}

fn git_output(mut command: Command, root: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let mut output = tempfile::tempfile().context("无法创建 Git 读取临时文件。")?;
    command.stdout(Stdio::from(output.try_clone()?));
    run_git_checked(command, root, Some((&output, max_bytes)))?;
    output.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    output.take(max_bytes + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= max_bytes,
        "Git 读取结果超过大小上限。"
    );
    Ok(bytes)
}

fn run_git_checked(
    mut command: Command,
    root: &Path,
    output_limit: Option<(&fs::File, u64)>,
) -> Result<()> {
    let child = command
        .stderr(Stdio::null())
        .spawn()
        .context("无法启动 Git，请先确认已安装 Git。")?;
    wait_git(child, root, output_limit)
}

fn wait_git(mut child: Child, root: &Path, output_limit: Option<(&fs::File, u64)>) -> Result<()> {
    let started = Instant::now();
    let result = loop {
        if let Some((output, limit)) = output_limit {
            match output.metadata() {
                Ok(metadata) if metadata.len() > limit => {
                    break Err(anyhow!("Git 读取结果超过大小上限。"))
                }
                Ok(_) => {}
                Err(error) => break Err(error.into()),
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    repository_size(root, &mut 0, &mut 0)
                } else {
                    Err(anyhow!(
                        "仓库下载失败；仅支持无需登录的公开 HTTPS Git 仓库。"
                    ))
                }
            }
            Ok(None) => {}
            Err(error) => break Err(error.into()),
        }
        if started.elapsed() >= GIT_TIMEOUT {
            break Err(anyhow!("仓库下载超过 60 秒上限，请选择较小的公开仓库。"));
        }
        if let Err(error) = repository_size(root, &mut 0, &mut 0) {
            break Err(error);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if result.is_err() {
        terminate_git(&mut child);
        let _ = child.wait();
    }
    result
}

fn unpack_skill_archive(reader: impl Read, destination: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(reader.take(MAX_IMPORT_BYTES + 4 * 1024 * 1024));
    let mut budget = CopyBudget::default();
    for entry in archive.entries().context("技能归档无效。")? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        anyhow::ensure!(
            !path.as_os_str().is_empty()
                && path
                    .components()
                    .all(|component| matches!(component, Component::Normal(_))),
            "技能归档包含不安全路径。"
        );
        if path
            .components()
            .any(|component| component.as_os_str() == ".git")
        {
            continue;
        }
        let kind = entry.header().entry_type();
        anyhow::ensure!(
            kind.is_file() || kind.is_dir(),
            "技能归档包含链接或特殊文件，无法导入。"
        );
        budget.add(if kind.is_file() { entry.size() } else { 0 })?;
        let target = destination.join(&path);
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            std::io::copy(&mut entry, &mut output)?;
            output.flush()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &target,
                    fs::Permissions::from_mode(entry.header().mode()? & 0o777),
                )?;
            }
        }
    }
    read_manifest(&manifest_path(destination)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(directory: &Path, name: &str) -> PathBuf {
        fs::create_dir_all(directory).unwrap();
        let manifest = directory.join(MANIFEST);
        fs::write(&manifest, format!("---\nname: {name}\ndescription: >-\n  Read code safely.\n  Never execute this document.\n---\n# Body\n")).unwrap();
        manifest
    }

    #[test]
    fn reads_bounded_metadata_and_content_without_execution() {
        let root = tempfile::tempdir().unwrap();
        let path = skill(&root.path().join("sample"), "sample-skill");
        let summary = summary_at(&path, "local", false).unwrap();
        assert_eq!(summary.name, "sample-skill");
        assert_eq!(
            summary.description,
            "Read code safely. Never execute this document."
        );
        assert!(!summary.can_toggle);
        assert!(read_manifest(&path).unwrap().contains("# Body"));
        let oversized = fs::OpenOptions::new().write(true).open(&path).unwrap();
        oversized.set_len(MAX_MANIFEST_BYTES + 1).unwrap();
        assert!(read_manifest(&path).is_err());
        assert!(manifest_path(&root.path().join("not-a-skill.txt")).is_err());
        assert_eq!(
            metadata(
                "---\r\nname: 'quoted-skill'\r\ndescription: \"Quoted description\"\r\n---\r\n",
                "fallback"
            ),
            ("quoted-skill".into(), "Quoted description".into())
        );
    }

    #[test]
    fn import_requires_closed_frontmatter_and_both_required_metadata_fields() {
        for content in [
            "# Just instructions\nname: sample\ndescription: missing frontmatter\n",
            "---\nname: sample\ndescription: no closing delimiter\n",
            "---\ndescription: missing name\n---\n",
            "---\nname: sample\n---\n",
            "---\nname: ''\ndescription: empty name\n---\n",
            "---\nname: sample\ndescription: >-\n---\n",
        ] {
            assert!(validate_import_manifest(content).is_err());
            let temporary = tempfile::tempdir().unwrap();
            let source = temporary.path().join("source");
            let destination = temporary.path().join("destination");
            fs::create_dir(&source).unwrap();
            fs::create_dir(&destination).unwrap();
            fs::write(source.join(MANIFEST), content).unwrap();
            assert!(import_local_at(
                &source,
                &destination,
                std::ffi::OsStr::new("source"),
                "global_agents",
                &DocumentMut::new()
            )
            .is_err());
            assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
            assert_eq!(fs::read_to_string(source.join(MANIFEST)).unwrap(), content);
        }
        assert!(validate_import_manifest("---\nname: 'quoted-name'\ndescription: >-\n  A folded\n  description.\n---\n# Instructions\n").is_ok());
    }

    #[test]
    fn malformed_metadata_still_appears_in_readonly_scan_with_warning() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("legacy-skill");
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join(MANIFEST),
            "# Legacy instructions without metadata\n",
        )
        .unwrap();
        let scan = scan_roots(
            &[(temporary.path().to_path_buf(), "local")],
            &DocumentMut::new(),
            false,
        )
        .unwrap();
        assert_eq!(scan.skills.len(), 1);
        assert_eq!(scan.skills[0].name, "legacy-skill");
        assert!(!scan.skills[0].can_toggle);
        assert_eq!(scan.warnings.len(), 1);
    }

    #[test]
    fn scans_selected_roots_and_applies_disabled_absolute_path() {
        let temporary = tempfile::tempdir().unwrap();
        let global = temporary.path().join("global");
        let legacy = temporary.path().join("legacy");
        let project = temporary.path().join("project/.agents/skills");
        let disabled = skill(&global.join("first"), "first")
            .canonicalize()
            .unwrap();
        let builtin = skill(&legacy.join(".system/builtin"), "builtin");
        let builtin_contents = fs::read(&builtin).unwrap();
        skill(&legacy.join("custom"), "custom");
        skill(&project.join("project-skill"), "project");
        skill(&global.join("node_modules/ignored"), "ignored");
        skill(&global.join(".codexbox-skill-unpublished"), "unpublished");
        let mut config = DocumentMut::new();
        set_config_enabled(&mut config, &disabled, false).unwrap();
        let scan = scan_roots(
            &[
                (global, "global_agents"),
                (legacy, "global_codex"),
                (project, "project_agents"),
            ],
            &config,
            true,
        )
        .unwrap();
        assert_eq!(scan.skills.len(), 3);
        assert!(scan
            .skills
            .iter()
            .any(|entry| entry.name == "first" && !entry.enabled && entry.can_toggle));
        assert!(scan
            .skills
            .iter()
            .any(|entry| entry.name == "custom" && entry.scope == "global_codex"));
        assert!(!scan.skills.iter().any(|entry| entry.name == "builtin"));
        assert!(scan
            .skills
            .iter()
            .any(|entry| entry.scope == "project_agents"));
        assert_eq!(fs::read(&builtin).unwrap(), builtin_contents);
        assert_eq!(
            read_manifest(&builtin).unwrap().as_bytes(),
            builtin_contents
        );
        let explicitly_selected = scan_roots(
            &[(
                builtin.parent().unwrap().parent().unwrap().to_path_buf(),
                "local",
            )],
            &DocumentMut::new(),
            false,
        )
        .unwrap();
        assert_eq!(explicitly_selected.skills.len(), 1);
        assert_eq!(explicitly_selected.skills[0].name, "builtin");
    }

    #[test]
    fn imports_one_skill_atomically_and_never_replaces_conflicts() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source/sample");
        let manifest = skill(&source, "sample");
        fs::create_dir(source.join("scripts")).unwrap();
        fs::write(source.join("scripts/run.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::create_dir(source.join(".git")).unwrap();
        fs::write(source.join(".git/config"), "source remains read-only").unwrap();
        let original = fs::read(&manifest).unwrap();
        let destination = temporary.path().join("destination");
        fs::create_dir(&destination).unwrap();
        let summary = import_local_at(
            &source,
            &destination,
            std::ffi::OsStr::new("sample"),
            "global_agents",
            &DocumentMut::new(),
        )
        .unwrap();
        assert_eq!(
            summary.skill_path,
            display_path(&destination.join("sample/SKILL.md"))
        );
        assert_eq!(fs::read(&manifest).unwrap(), original);
        assert_eq!(
            fs::read(destination.join("sample/SKILL.md")).unwrap(),
            original
        );
        assert!(destination.join("sample/scripts/run.sh").is_file());
        assert!(!destination.join("sample/.git").exists());
        fs::write(destination.join("sample/preserve.txt"), "keep").unwrap();
        assert!(import_local_at(
            &source,
            &destination,
            std::ffi::OsStr::new("sample"),
            "global_agents",
            &DocumentMut::new()
        )
        .is_err());
        assert_eq!(
            fs::read_to_string(destination.join("sample/preserve.txt")).unwrap(),
            "keep"
        );
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
    }

    #[test]
    fn size_failure_cleans_stage_and_rejects_traversal_names() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        skill(&source, "large");
        fs::File::create(source.join("large.bin"))
            .unwrap()
            .set_len(MAX_IMPORT_BYTES + 1)
            .unwrap();
        let destination = temporary.path().join("destination");
        fs::create_dir(&destination).unwrap();
        assert!(import_local_at(
            &source,
            &destination,
            std::ffi::OsStr::new("large"),
            "global_agents",
            &DocumentMut::new()
        )
        .is_err());
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        fs::remove_file(source.join("large.bin")).unwrap();
        assert!(import_local_at(
            &source,
            &destination,
            std::ffi::OsStr::new("../escape"),
            "global_agents",
            &DocumentMut::new()
        )
        .is_err());
        assert!(!temporary.path().join("escape").exists());
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        let mut budget = CopyBudget {
            files: MAX_IMPORT_FILES,
            bytes: 0,
        };
        assert!(budget.add(0).is_err());
    }

    #[test]
    fn no_clobber_publish_rejects_even_an_existing_empty_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        assert!(rename_without_replace(&source, &destination).is_err());
        assert!(source.exists() && destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn scans_symlink_directories_once_but_import_rejects_all_links() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("actual");
        let manifest = skill(&source, "linked");
        let roots = temporary.path().join("roots");
        fs::create_dir(&roots).unwrap();
        symlink(&source, roots.join("alias")).unwrap();
        symlink(&source, roots.join("other-alias")).unwrap();
        symlink(&roots, roots.join("cycle")).unwrap();
        let scan = scan_roots(&[(roots, "global_agents")], &DocumentMut::new(), true).unwrap();
        assert_eq!(scan.skills.len(), 1);
        assert_eq!(
            scan.skills[0].skill_path,
            display_path(&manifest.canonicalize().unwrap())
        );
        let outside = temporary.path().join("outside");
        fs::write(&outside, "must not be copied").unwrap();
        symlink(&outside, source.join("escape")).unwrap();
        let destination = temporary.path().join("destination");
        fs::create_dir(&destination).unwrap();
        assert!(import_local_at(
            &source,
            &destination,
            std::ffi::OsStr::new("linked"),
            "global_agents",
            &DocumentMut::new()
        )
        .is_err());
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        assert_eq!(fs::read_to_string(outside).unwrap(), "must not be copied");
        let alias = temporary.path().join("SKILL.md");
        symlink(&manifest, &alias).unwrap();
        assert!(manifest_path(&alias).is_err());
    }

    #[test]
    fn disabling_preserves_skill_files_config_comments_and_other_servers() {
        let temporary = tempfile::tempdir().unwrap();
        let manifest = skill(&temporary.path().join("skill"), "preserved")
            .canonicalize()
            .unwrap();
        let original = fs::read(&manifest).unwrap();
        let source = format!("# existing user comment\nmodel = \"example-model\"\n\n[model_providers.custom]\nbase_url = \"https://example.invalid/v1\"\n\n[mcp_servers.docs]\ncommand = \"existing-tool\"\n\n[[skills.config]]\npath = {}\nenabled = true # preserve this note\ncustom = \"keep\"\n", toml_edit::Value::from(display_path(&manifest)));
        let mut document = source.parse::<DocumentMut>().unwrap();
        set_config_enabled(&mut document, &manifest, false).unwrap();
        let saved = document.to_string();
        assert!(saved.starts_with("# existing user comment\nmodel = \"example-model\""));
        assert!(saved.contains("[model_providers.custom]"));
        assert!(saved.contains("command = \"existing-tool\""));
        assert!(saved.contains("enabled = false # preserve this note"));
        assert!(saved.contains("custom = \"keep\""));
        assert_eq!(
            config_overrides(&document).unwrap().get(&manifest),
            Some(&false)
        );
        set_config_enabled(&mut document, &manifest, true).unwrap();
        assert_eq!(
            document["skills"]["config"]
                .as_array_of_tables()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(fs::read(manifest).unwrap(), original);
    }

    #[test]
    fn inline_skill_config_is_preserved_and_invalid_shape_is_rejected() {
        let path = Path::new("/example/skill/SKILL.md");
        let mut doc = "model = 'example'\n[skills]\nconfig = [{ path = '/example/skill/SKILL.md', enabled = true, future = 'keep' }]\n".parse::<DocumentMut>().unwrap();
        set_config_enabled(&mut doc, path, false).unwrap();
        assert!(doc.to_string().contains("future = 'keep'"));
        assert_eq!(config_overrides(&doc).unwrap().get(path), Some(&false));
        let original = "[skills]\nconfig = 'unknown future shape'\n";
        let mut doc = original.parse::<DocumentMut>().unwrap();
        assert!(set_config_enabled(&mut doc, path, false).is_err());
        assert_eq!(doc.to_string(), original);
    }

    #[test]
    fn validates_public_git_url_and_relative_subdirectory_without_cloning() {
        assert!(public_repository_url("https://github.com/example/skills.git").is_ok());
        for url in [
            "file:///tmp/skills",
            "http://example.invalid/skills",
            "ssh://example.invalid/skills",
            "https://someone@example.invalid/skills",
            "https://example.invalid/skills?token=x",
            "https://example.invalid/skills#main",
        ] {
            assert!(public_repository_url(url).is_err());
        }
        assert_eq!(
            safe_subdirectory("skills/review").unwrap(),
            PathBuf::from("skills/review")
        );
        assert_eq!(safe_subdirectory(".").unwrap(), PathBuf::new());
        for path in [
            "../escape",
            "/absolute",
            "skills/../../escape",
            "skills\\escape",
        ] {
            assert!(safe_subdirectory(path).is_err());
        }
    }

    fn git_tree_entry(mode: &str, kind: &str, path: &str) -> String {
        format!("{mode} {kind} {}\t{path}\0", "a".repeat(40))
    }

    #[cfg(unix)]
    #[test]
    fn completed_git_child_still_checks_final_repository_size() {
        let temporary = tempfile::tempdir().unwrap();
        let oversized = temporary.path().join("oversized-pack");
        let mut child = Command::new("/bin/sh")
            .args([
                "-c",
                "dd if=/dev/zero of=\"$1\" bs=1 count=0 seek=\"$2\" 2>/dev/null",
                "headroom-sparse-file-test",
            ])
            .arg(&oversized)
            .arg((MAX_REPOSITORY_BYTES + 1).to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // Force the completed-before-first-poll case without timing or network dependencies.
        assert!(child.wait().unwrap().success());
        assert_eq!(
            fs::metadata(&oversized).unwrap().len(),
            MAX_REPOSITORY_BYTES + 1
        );
        let error = wait_git(child, temporary.path(), None).unwrap_err();
        assert!(error.to_string().contains("128 MiB"));
    }

    #[test]
    fn git_discovery_finds_root_and_nested_skills_without_checkout() {
        let listing = [
            git_tree_entry("100644", "blob", "README.md"),
            git_tree_entry("100644", "blob", MANIFEST),
            git_tree_entry("100755", "blob", "skills/review code/SKILL.md"),
            git_tree_entry("120000", "blob", "linked/SKILL.md"),
            git_tree_entry("160000", "commit", "submodule"),
        ]
        .concat();
        let (manifests, warnings) = git_skill_manifests(listing.as_bytes()).unwrap();
        assert_eq!(manifests.len(), 2);
        assert_eq!(manifests[0].subdirectory, ".");
        assert_eq!(manifests[1].subdirectory, "skills/review code");
        assert_eq!(manifests[0].object_id, "a".repeat(40));
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn git_discovery_rejects_unsafe_paths_and_caps_candidates() {
        let listing = [
            "../escape/SKILL.md",
            "/absolute/SKILL.md",
            "skills/../../escape/SKILL.md",
            "skills\\escape/SKILL.md",
            " leading/SKILL.md",
            "trailing /SKILL.md",
        ]
        .into_iter()
        .map(|path| git_tree_entry("100644", "blob", path))
        .collect::<String>();
        let (manifests, warnings) = git_skill_manifests(listing.as_bytes()).unwrap();
        assert!(manifests.is_empty());
        assert_eq!(warnings.len(), 6);

        let listing = (0..=MAX_SKILLS)
            .map(|index| git_tree_entry("100644", "blob", &format!("skill-{index}/SKILL.md")))
            .collect::<String>();
        let (manifests, warnings) = git_skill_manifests(listing.as_bytes()).unwrap();
        assert_eq!(manifests.len(), MAX_SKILLS);
        assert_eq!(warnings.len(), 1);
        assert!(git_skill_manifests(b"not a git tree\0").is_err());
    }

    #[test]
    fn git_discovery_returns_validated_metadata_and_exact_import_subdirectory() {
        let temporary = tempfile::tempdir().unwrap();
        let (root, _) = resolve_import_root(
            SkillTarget::Project,
            Some(temporary.path().to_str().unwrap()),
        )
        .unwrap();
        let url = public_repository_url("https://example.invalid/collection.git").unwrap();
        let content = b"---\nname: 'code-review'\ndescription: >-\n  Review source\n  without executing it.\n---\n# Instructions\n";
        let skill = git_skill_summary("skills/review code", content, &url, &root).unwrap();
        assert_eq!(skill.name, "code-review");
        assert_eq!(skill.description, "Review source without executing it.");
        assert_eq!(
            safe_subdirectory(&skill.subdirectory).unwrap(),
            PathBuf::from("skills/review code")
        );
        let encoded = serde_json::to_value(skill).unwrap();
        assert_eq!(encoded["subdirectory"], "skills/review code");
        assert_eq!(
            encoded["installDirectory"],
            display_path(&root.join("review code"))
        );
        let root_skill = git_skill_summary(".", content, &url, &root).unwrap();
        assert_eq!(
            root_skill.install_directory,
            display_path(&root.join("collection"))
        );
        assert_eq!(root_skill.name, "code-review");
        assert!(!temporary.path().join(".agents").exists());
        assert!(git_skill_summary(".", b"# No frontmatter", &url, &root).is_err());
        assert!(
            git_skill_summary(".", b"---\nname: missing-description\n---\n", &url, &root).is_err()
        );
        assert!(git_skill_summary(".", &[0xff], &url, &root).is_err());
        assert!(git_skill_summary(
            ".",
            &vec![b'x'; MAX_MANIFEST_BYTES as usize + 1],
            &url,
            &root
        )
        .is_err());
    }

    fn archive_file(builder: &mut tar::Builder<Vec<u8>>, path: &str, content: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, content).unwrap();
    }

    #[test]
    fn extracts_only_safe_bounded_skill_archives() {
        let temporary = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        archive_file(
            &mut builder,
            MANIFEST,
            b"---\nname: archived\ndescription: imported\n---\n",
        );
        archive_file(&mut builder, "references/readme.txt", b"reference text");
        let bytes = builder.into_inner().unwrap();
        unpack_skill_archive(bytes.as_slice(), temporary.path()).unwrap();
        assert_eq!(
            metadata(
                &read_manifest(&temporary.path().join(MANIFEST)).unwrap(),
                "fallback"
            )
            .0,
            "archived"
        );
        assert_eq!(
            fs::read_to_string(temporary.path().join("references/readme.txt")).unwrap(),
            "reference text"
        );

        let temporary = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_link_name("../../outside").unwrap();
        header.set_cksum();
        builder
            .append_data(&mut header, "escape", std::io::empty())
            .unwrap();
        let bytes = builder.into_inner().unwrap();
        assert!(unpack_skill_archive(bytes.as_slice(), temporary.path()).is_err());
        assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 0);
    }
}
