use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::config::{self, Paths, RepoEntry};
use crate::filelock;
use crate::giturl;
use crate::mirror;
use crate::workspace;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Template {
    pub repos: Vec<TemplateRepo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateRepo {
    pub url: String,
}

impl Template {
    /// Derive identities from repo URLs using giturl::parse.
    pub fn identities(&self) -> Result<Vec<String>> {
        self.repos
            .iter()
            .map(|r| {
                let parsed = giturl::parse(&r.url)?;
                Ok(parsed.identity())
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Source classification
// ---------------------------------------------------------------------------

/// Classifies a user-provided source string into a name or file path.
/// Unambiguous because template names cannot contain `/`, `\`, or end in `.yaml`.
#[derive(Debug, PartialEq)]
pub enum TemplateSource {
    Name(String),
    FilePath(PathBuf),
}

pub fn classify_source(source: &str) -> TemplateSource {
    if source.contains('/') || source.contains('\\') || source.ends_with(".yaml") {
        TemplateSource::FilePath(PathBuf::from(source))
    } else {
        TemplateSource::Name(source.to_string())
    }
}

// ---------------------------------------------------------------------------
// Name validation
// ---------------------------------------------------------------------------

/// Validate a template name for safe use as a filesystem component.
/// Same rules as workspace::validate_name — no path separators, traversal, or special prefixes.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("template name cannot be empty");
    }
    if name.contains('\0') {
        bail!("template name cannot contain null bytes");
    }
    if name.contains('/') || name.contains('\\') {
        bail!("template name {:?} cannot contain path separators", name);
    }
    if name.contains("..") {
        bail!("template name {:?} cannot contain \"..\"", name);
    }
    if name.starts_with('-') {
        bail!("template name {:?} cannot start with a dash", name);
    }
    if name.starts_with('.') {
        bail!("template name {:?} cannot start with a dot", name);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Stored template CRUD
// ---------------------------------------------------------------------------

fn template_path(templates_dir: &Path, name: &str) -> PathBuf {
    templates_dir.join(format!("{}.yaml", name))
}

pub fn save(templates_dir: &Path, name: &str, template: &Template) -> Result<()> {
    validate_name(name)?;
    fs::create_dir_all(templates_dir)?;
    let path = template_path(templates_dir, name);
    let data = serde_yaml_ng::to_string(template)?;
    let mut tmp = tempfile::NamedTempFile::new_in(templates_dir)
        .context("creating temp file for template")?;
    tmp.write_all(data.as_bytes())
        .context("writing template to temp file")?;
    tmp.persist(&path)
        .context("renaming temp file to template")?;
    Ok(())
}

pub fn load(templates_dir: &Path, name: &str) -> Result<Template> {
    validate_name(name)?;
    let path = template_path(templates_dir, name);
    if !path.exists() {
        bail!("template {:?} not found", name);
    }
    load_from_file(&path)
}

pub fn delete(templates_dir: &Path, name: &str) -> Result<()> {
    validate_name(name)?;
    let path = template_path(templates_dir, name);
    // Use remove_file directly to avoid TOCTOU race with exists() check
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("template {:?} not found", name)
        }
        Err(e) => Err(e).with_context(|| format!("removing template {:?}", name)),
    }
}

pub fn list(templates_dir: &Path) -> Result<Vec<String>> {
    if !templates_dir.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(templates_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "yaml")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            names.push(stem.to_string());
        }
    }
    names.sort();
    Ok(names)
}

pub fn exists(templates_dir: &Path, name: &str) -> bool {
    validate_name(name).is_ok() && template_path(templates_dir, name).exists()
}

// ---------------------------------------------------------------------------
// Load from file
// ---------------------------------------------------------------------------

/// Load a template from a local file path.
pub fn load_from_file(path: &Path) -> Result<Template> {
    let data = fs::read_to_string(path).with_context(|| format!("reading template {:?}", path))?;
    let t: Template =
        serde_yaml_ng::from_str(&data).with_context(|| format!("parsing template {:?}", path))?;
    if t.repos.is_empty() {
        bail!("template {:?} has no repos", path);
    }
    Ok(t)
}

/// Serialize a template to YAML string.
pub fn to_yaml(template: &Template) -> Result<String> {
    serde_yaml_ng::to_string(template).context("serializing template")
}

// ---------------------------------------------------------------------------
// Workspace derivation
// ---------------------------------------------------------------------------

/// Create a template from an existing workspace's repo set.
pub fn from_workspace(paths: &Paths, ws_name: &str) -> Result<Template> {
    let ws_dir = workspace::dir(&paths.workspaces_dir, ws_name);
    let meta = workspace::load_metadata(&ws_dir)
        .with_context(|| format!("loading workspace {:?}", ws_name))?;
    let cfg = config::Config::load_from(&paths.config_path)?;

    let mut repos = Vec::new();
    for identity in meta.repos.keys() {
        let url = cfg
            .upstream_url(identity)
            .ok_or_else(|| anyhow::anyhow!("repo {:?} not in registry", identity))?;
        repos.push(TemplateRepo {
            url: url.to_string(),
        });
    }

    if repos.is_empty() {
        bail!("workspace {:?} has no repos", ws_name);
    }

    Ok(Template { repos })
}

// ---------------------------------------------------------------------------
// Auto-registration
// ---------------------------------------------------------------------------

/// Auto-register any repos from a template that aren't already in the registry.
/// Clones mirrors and adds entries to config.
pub fn auto_register(tmpl: &Template, cfg: &mut config::Config, paths: &Paths) -> Result<()> {
    let mut to_register = Vec::new();

    for repo in &tmpl.repos {
        let parsed = giturl::parse(&repo.url)?;
        let identity = parsed.identity();
        if !cfg.repos.contains_key(&identity) {
            to_register.push((identity, parsed, repo.url.clone()));
        }
    }

    if to_register.is_empty() {
        return Ok(());
    }

    eprintln!(
        "Auto-registering {} repos from template...",
        to_register.len()
    );

    for (identity, parsed, url) in &to_register {
        if !mirror::exists(&paths.mirrors_dir, parsed) {
            eprintln!("  cloning {}...", url);
            mirror::clone(&paths.mirrors_dir, parsed, url)
                .map_err(|e| anyhow::anyhow!("cloning {}: {}", identity, e))?;
        }
    }

    // Register under lock
    filelock::with_config(&paths.config_path, |locked_cfg| {
        for (identity, _, url) in &to_register {
            if !locked_cfg.repos.contains_key(identity) {
                locked_cfg.repos.insert(
                    identity.clone(),
                    RepoEntry {
                        url: url.clone(),
                        added: Utc::now(),
                    },
                );
            }
        }
        Ok(())
    })?;

    // Update the in-memory config to reflect the new repos
    for (identity, _, url) in to_register {
        cfg.repos.insert(
            identity,
            RepoEntry {
                url,
                added: Utc::now(),
            },
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Group migration
// ---------------------------------------------------------------------------

/// Migrate a single group to a template file. Looks up repo URLs from config.
/// Skips if a template with the same name already exists.
/// Returns true if a new template was created.
pub fn migrate_group(
    templates_dir: &Path,
    cfg: &config::Config,
    group_name: &str,
    repo_identities: &[String],
) -> Result<bool> {
    if exists(templates_dir, group_name) {
        return Ok(false);
    }

    let mut repos = Vec::new();
    for identity in repo_identities {
        let url = cfg
            .upstream_url(identity)
            .ok_or_else(|| anyhow::anyhow!("repo {:?} not in registry", identity))?;
        repos.push(TemplateRepo {
            url: url.to_string(),
        });
    }

    if repos.is_empty() {
        return Ok(false);
    }

    save(templates_dir, group_name, &Template { repos })?;
    Ok(true)
}

/// Migrate all groups from config to template files. Returns the count of migrated groups.
pub fn migrate_all_groups(templates_dir: &Path, cfg: &config::Config) -> Result<usize> {
    let mut count = 0;
    for (name, entry) in &cfg.groups {
        match migrate_group(templates_dir, cfg, name, &entry.repos) {
            Ok(true) => {
                eprintln!("  migrated group {:?} to template", name);
                count += 1;
            }
            Ok(false) => {} // already exists, skip silently
            Err(e) => {
                eprintln!("  warning: failed to migrate group {:?}: {}", name, e);
            }
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_template() -> Template {
        Template {
            repos: vec![
                TemplateRepo {
                    url: "git@github.com:acme/api-gateway.git".into(),
                },
                TemplateRepo {
                    url: "git@github.com:acme/user-service.git".into(),
                },
            ],
        }
    }

    #[test]
    fn save_and_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("templates");

        let t = sample_template();
        save(&dir, "dash", &t).unwrap();

        let loaded = load(&dir, "dash").unwrap();
        assert_eq!(loaded.repos.len(), 2);
        assert_eq!(loaded.repos[0].url, "git@github.com:acme/api-gateway.git");
        assert_eq!(loaded.repos[1].url, "git@github.com:acme/user-service.git");
    }

    #[test]
    fn load_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let err = load(tmp.path(), "nonexistent").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn delete_removes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("templates");

        save(&dir, "dash", &sample_template()).unwrap();
        assert!(exists(&dir, "dash"));

        delete(&dir, "dash").unwrap();
        assert!(!exists(&dir, "dash"));
    }

    #[test]
    fn delete_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let err = delete(tmp.path(), "nonexistent").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn list_templates() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("templates");

        assert!(list(&dir).unwrap().is_empty());

        save(&dir, "backend", &sample_template()).unwrap();
        save(&dir, "frontend", &sample_template()).unwrap();

        let names = list(&dir).unwrap();
        assert_eq!(names, vec!["backend", "frontend"]);
    }

    #[test]
    fn identities_from_template() {
        let t = sample_template();
        let ids = t.identities().unwrap();
        assert_eq!(
            ids,
            vec![
                "github.com/acme/api-gateway",
                "github.com/acme/user-service",
            ]
        );
    }

    #[test]
    fn load_empty_repos_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("templates");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("empty.yaml"), "repos: []\n").unwrap();

        let err = load(&dir, "empty").unwrap_err();
        assert!(err.to_string().contains("no repos"));
    }

    #[test]
    fn validate_name_rejects_traversal() {
        struct Case {
            name: &'static str,
            input: &'static str,
            want_err: bool,
        }

        let cases = vec![
            Case {
                name: "valid simple name",
                input: "backend",
                want_err: false,
            },
            Case {
                name: "valid with hyphens",
                input: "my-team-backend",
                want_err: false,
            },
            Case {
                name: "empty",
                input: "",
                want_err: true,
            },
            Case {
                name: "path traversal",
                input: "../../etc/passwd",
                want_err: true,
            },
            Case {
                name: "dot-dot in middle",
                input: "foo..bar",
                want_err: true,
            },
            Case {
                name: "forward slash",
                input: "foo/bar",
                want_err: true,
            },
            Case {
                name: "backslash",
                input: "foo\\bar",
                want_err: true,
            },
            Case {
                name: "starts with dot",
                input: ".hidden",
                want_err: true,
            },
            Case {
                name: "starts with dash",
                input: "-flag",
                want_err: true,
            },
            Case {
                name: "null byte",
                input: "foo\0bar",
                want_err: true,
            },
        ];

        for tc in cases {
            let result = validate_name(tc.input);
            assert_eq!(result.is_err(), tc.want_err, "case: {}", tc.name);
        }
    }

    #[test]
    fn classify_source_cases() {
        struct Case {
            name: &'static str,
            input: &'static str,
            want: TemplateSource,
        }

        let cases = vec![
            Case {
                name: "plain name",
                input: "backend",
                want: TemplateSource::Name("backend".into()),
            },
            Case {
                name: "name with hyphens",
                input: "my-team-backend",
                want: TemplateSource::Name("my-team-backend".into()),
            },
            Case {
                name: "relative file path",
                input: "./backend.wsp.yaml",
                want: TemplateSource::FilePath("./backend.wsp.yaml".into()),
            },
            Case {
                name: "absolute file path",
                input: "/tmp/backend.yaml",
                want: TemplateSource::FilePath("/tmp/backend.yaml".into()),
            },
            Case {
                name: "file ending in .yaml",
                input: "backend.yaml",
                want: TemplateSource::FilePath("backend.yaml".into()),
            },
            Case {
                name: "file with slash no extension",
                input: "path/to/template",
                want: TemplateSource::FilePath("path/to/template".into()),
            },
        ];

        for tc in cases {
            let got = classify_source(tc.input);
            assert_eq!(got, tc.want, "case: {}", tc.name);
        }
    }

    #[test]
    fn load_from_file_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.wsp.yaml");

        let t = sample_template();
        let yaml = to_yaml(&t).unwrap();
        std::fs::write(&path, &yaml).unwrap();

        let loaded = load_from_file(&path).unwrap();
        assert_eq!(loaded.repos.len(), 2);
        assert_eq!(loaded.repos[0].url, t.repos[0].url);
    }

    #[test]
    fn load_from_file_missing() {
        let result = load_from_file(Path::new("/nonexistent/template.yaml"));
        assert!(result.is_err());
    }

    #[test]
    fn load_from_file_empty_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("empty.yaml");
        std::fs::write(&path, "repos: []\n").unwrap();

        let err = load_from_file(&path).unwrap_err();
        assert!(err.to_string().contains("no repos"));
    }

    #[test]
    fn to_yaml_round_trip() {
        let t = sample_template();
        let yaml = to_yaml(&t).unwrap();
        let parsed: Template = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(parsed.repos.len(), t.repos.len());
        assert_eq!(parsed.repos[0].url, t.repos[0].url);
    }

    fn sample_config() -> config::Config {
        use chrono::Utc;
        use std::collections::BTreeMap;
        let mut cfg = config::Config::default();
        cfg.repos.insert(
            "github.com/acme/api-gateway".into(),
            RepoEntry {
                url: "git@github.com:acme/api-gateway.git".into(),
                added: Utc::now(),
            },
        );
        cfg.repos.insert(
            "github.com/acme/user-service".into(),
            RepoEntry {
                url: "git@github.com:acme/user-service.git".into(),
                added: Utc::now(),
            },
        );
        cfg.groups.insert(
            "backend".into(),
            config::GroupEntry {
                repos: vec![
                    "github.com/acme/api-gateway".into(),
                    "github.com/acme/user-service".into(),
                ],
            },
        );
        cfg.groups.insert(
            "frontend".into(),
            config::GroupEntry {
                repos: vec!["github.com/acme/api-gateway".into()],
            },
        );
        cfg
    }

    #[test]
    fn migrate_group_creates_template() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("templates");
        let cfg = sample_config();

        let created = migrate_group(&dir, &cfg, "backend", &cfg.groups["backend"].repos).unwrap();
        assert!(created);

        let t = load(&dir, "backend").unwrap();
        assert_eq!(t.repos.len(), 2);
        assert_eq!(t.repos[0].url, "git@github.com:acme/api-gateway.git");
    }

    #[test]
    fn migrate_group_skips_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("templates");
        let cfg = sample_config();

        // Pre-create a template with the same name
        save(&dir, "backend", &sample_template()).unwrap();

        let created = migrate_group(&dir, &cfg, "backend", &cfg.groups["backend"].repos).unwrap();
        assert!(!created);
    }

    #[test]
    fn migrate_all_groups_creates_templates() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("templates");
        let cfg = sample_config();

        let count = migrate_all_groups(&dir, &cfg).unwrap();
        assert_eq!(count, 2);

        assert!(exists(&dir, "backend"));
        assert!(exists(&dir, "frontend"));
    }
}
