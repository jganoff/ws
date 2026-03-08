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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<TemplateConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_md: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TemplateConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_integrations: Option<std::collections::BTreeMap<String, bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_strategy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateRepo {
    pub url: String,
}

impl Template {
    /// Apply template config onto global config, returning a modified copy.
    /// Template config overrides global config; absent template fields leave config unchanged.
    pub fn apply_config(&self, cfg: &config::Config) -> config::Config {
        let mut effective = cfg.clone();
        if let Some(ref settings) = self.config {
            if let Some(ref li) = settings.language_integrations {
                let target = effective
                    .language_integrations
                    .get_or_insert_with(std::collections::BTreeMap::new);
                for (k, v) in li {
                    target.insert(k.clone(), *v);
                }
            }
            if let Some(ref strategy) = settings.sync_strategy {
                effective.sync_strategy = Some(strategy.clone());
            }
        }
        effective
    }

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
// Validation
// ---------------------------------------------------------------------------

/// Reject agent_md content that contains wsp markers, which would corrupt
/// the marker-based section management in AGENTS.md.
fn validate_agent_md(tmpl: &Template) -> Result<()> {
    if let Some(ref content) = tmpl.agent_md
        && (content.contains(crate::agentmd::MARKER_BEGIN)
            || content.contains(crate::agentmd::MARKER_END))
    {
        bail!("agent_md content cannot contain wsp markers (<!-- wsp:begin/end -->)");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Load from file
// ---------------------------------------------------------------------------

/// Load a template from a local file path.
/// Accepts both the template format (`repos: [{url: ...}]`) and the
/// .wsp.yaml metadata format (`repos: {identity: {url: ...}}`).
pub fn load_from_file(path: &Path) -> Result<Template> {
    let data = fs::read_to_string(path).with_context(|| format!("reading template {:?}", path))?;

    // Try template format first
    let tmpl_err = match serde_yaml_ng::from_str::<Template>(&data) {
        Ok(t) => {
            if t.repos.is_empty() {
                bail!("template {:?} has no repos", path);
            }
            validate_agent_md(&t)?;
            return Ok(t);
        }
        Err(e) => e,
    };

    // Try .wsp.yaml metadata format
    let tmpl = match serde_yaml_ng::from_str::<workspace::Metadata>(&data) {
        Ok(meta) => template_from_metadata(&meta)?,
        Err(meta_err) => bail!(
            "could not parse {:?}:\n  as template: {}\n  as .wsp.yaml: {}",
            path,
            tmpl_err,
            meta_err
        ),
    };
    validate_agent_md(&tmpl)?;
    Ok(tmpl)
}

/// Convert a .wsp.yaml Metadata into a Template by extracting repo URLs.
fn template_from_metadata(meta: &workspace::Metadata) -> Result<Template> {
    let mut repos = Vec::new();
    for (identity, repo_ref) in &meta.repos {
        let url = repo_ref
            .as_ref()
            .and_then(|r| r.url.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "repo {:?} in .wsp.yaml has no URL — cannot use as template",
                    identity
                )
            })?;
        repos.push(TemplateRepo { url });
    }
    if repos.is_empty() {
        bail!("no repos in .wsp.yaml");
    }
    Ok(Template {
        repos,
        config: None,
        agent_md: None,
    })
}

/// Serialize a template to YAML string.
pub fn to_yaml(template: &Template) -> Result<String> {
    serde_yaml_ng::to_string(template).context("serializing template")
}

// ---------------------------------------------------------------------------
// Workspace derivation
// ---------------------------------------------------------------------------

/// Create a template from an existing workspace's repo set.
/// Uses URLs from .wsp.yaml if available, falls back to registry.
pub fn from_workspace(paths: &Paths, ws_name: &str) -> Result<Template> {
    let ws_dir = workspace::dir(&paths.workspaces_dir, ws_name);
    let meta = workspace::load_metadata(&ws_dir)
        .with_context(|| format!("loading workspace {:?}", ws_name))?;
    let cfg = config::Config::load_from(&paths.config_path)?;

    let mut repos = Vec::new();
    for (identity, repo_ref) in &meta.repos {
        // Prefer URL from .wsp.yaml, fall back to registry
        let url = repo_ref
            .as_ref()
            .and_then(|r| r.url.clone())
            .or_else(|| cfg.upstream_url(identity).map(|s| s.to_string()))
            .ok_or_else(|| {
                anyhow::anyhow!("repo {:?} has no URL in .wsp.yaml or registry", identity)
            })?;
        repos.push(TemplateRepo { url });
    }

    if repos.is_empty() {
        bail!("workspace {:?} has no repos", ws_name);
    }

    // Extract user-written AGENTS.md content if present
    let agent_md = {
        let agents_path = ws_dir.join("AGENTS.md");
        if agents_path.exists() {
            let content = fs::read_to_string(&agents_path).ok().unwrap_or_default();
            crate::agentmd::extract_user_content(&content)
        } else {
            None
        }
    };

    Ok(Template {
        repos,
        config: None,
        agent_md,
    })
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

    save(
        templates_dir,
        group_name,
        &Template {
            repos,
            config: None,
            agent_md: None,
        },
    )?;
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
            config: None,
            agent_md: None,
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
    fn load_from_file_accepts_wsp_yaml_format() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("workspace.wsp.yaml");
        std::fs::write(
            &path,
            r#"
name: my-feature
branch: my-feature
repos:
  github.com/acme/api-gateway:
    url: git@github.com:acme/api-gateway.git
  github.com/acme/user-service:
    url: git@github.com:acme/user-service.git
created: 2026-03-07T10:00:00Z
"#,
        )
        .unwrap();

        let loaded = load_from_file(&path).unwrap();
        assert_eq!(loaded.repos.len(), 2);
        // URLs are extracted from the metadata format
        let urls: Vec<&str> = loaded.repos.iter().map(|r| r.url.as_str()).collect();
        assert!(urls.contains(&"git@github.com:acme/api-gateway.git"));
        assert!(urls.contains(&"git@github.com:acme/user-service.git"));
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

    #[test]
    fn apply_config_overrides_config() {
        use std::collections::BTreeMap;

        let mut cfg = config::Config::default();
        cfg.sync_strategy = Some("rebase".into());
        let mut li = BTreeMap::new();
        li.insert("go".into(), false);
        cfg.language_integrations = Some(li);

        let tmpl = Template {
            repos: vec![],
            config: Some(TemplateConfig {
                language_integrations: Some(BTreeMap::from([("go".into(), true)])),
                sync_strategy: Some("merge".into()),
            }),
            agent_md: None,
        };

        let effective = tmpl.apply_config(&cfg);
        assert_eq!(effective.sync_strategy.as_deref(), Some("merge"));
        assert_eq!(
            effective.language_integrations.as_ref().unwrap()["go"],
            true
        );
    }

    #[test]
    fn apply_config_preserves_config_when_absent() {
        use std::collections::BTreeMap;

        let mut cfg = config::Config::default();
        cfg.sync_strategy = Some("rebase".into());
        let mut li = BTreeMap::new();
        li.insert("go".into(), true);
        cfg.language_integrations = Some(li);

        let tmpl = Template {
            repos: vec![],
            config: None,
            agent_md: None,
        };

        let effective = tmpl.apply_config(&cfg);
        assert_eq!(effective.sync_strategy.as_deref(), Some("rebase"));
        assert_eq!(
            effective.language_integrations.as_ref().unwrap()["go"],
            true
        );
    }

    #[test]
    fn settings_round_trip_yaml() {
        use std::collections::BTreeMap;

        let tmpl = Template {
            repos: vec![TemplateRepo {
                url: "git@github.com:acme/api.git".into(),
            }],
            config: Some(TemplateConfig {
                language_integrations: Some(BTreeMap::from([("go".into(), true)])),
                sync_strategy: Some("merge".into()),
            }),
            agent_md: None,
        };

        let yaml = to_yaml(&tmpl).unwrap();
        let parsed: Template = serde_yaml_ng::from_str(&yaml).unwrap();

        let s = parsed.config.unwrap();
        assert_eq!(s.sync_strategy.as_deref(), Some("merge"));
        assert_eq!(s.language_integrations.as_ref().unwrap()["go"], true);
    }

    #[test]
    fn agent_md_round_trip_yaml() {
        let tmpl = Template {
            repos: vec![TemplateRepo {
                url: "git@github.com:acme/api.git".into(),
            }],
            config: None,
            agent_md: Some("# Project Rules\n\nAlways use table-driven tests.".into()),
        };

        let yaml = to_yaml(&tmpl).unwrap();
        let parsed: Template = serde_yaml_ng::from_str(&yaml).unwrap();

        assert_eq!(
            parsed.agent_md.as_deref(),
            Some("# Project Rules\n\nAlways use table-driven tests.")
        );
    }

    #[test]
    fn agent_md_none_omitted_from_yaml() {
        let tmpl = Template {
            repos: vec![TemplateRepo {
                url: "git@github.com:acme/api.git".into(),
            }],
            config: None,
            agent_md: None,
        };

        let yaml = to_yaml(&tmpl).unwrap();
        assert!(!yaml.contains("agent_md"));
    }

    #[test]
    fn extract_user_content_cases() {
        use crate::agentmd::{MARKER_BEGIN, MARKER_END};

        struct Case {
            name: &'static str,
            input: String,
            want: Option<&'static str>,
        }

        let cases = vec![
            Case {
                name: "user content before markers",
                input: format!(
                    "# My Project\n\nCustom rules here.\n\n{}\ngenerated\n{}\n",
                    MARKER_BEGIN, MARKER_END
                ),
                want: Some("# My Project\n\nCustom rules here."),
            },
            Case {
                name: "only markers, no user content",
                input: format!("{}\ngenerated\n{}\n", MARKER_BEGIN, MARKER_END),
                want: None,
            },
            Case {
                name: "default placeholder only",
                input: format!(
                    "# Workspace: test\n\n<!-- Add your project-specific notes for AI agents here -->\n\n{}\ngenerated\n{}\n",
                    MARKER_BEGIN, MARKER_END
                ),
                want: Some("# Workspace: test"),
            },
            Case {
                name: "no markers at all",
                input: "# Custom content\n\nSome notes.".into(),
                want: Some("# Custom content\n\nSome notes."),
            },
            Case {
                name: "empty file",
                input: "".into(),
                want: None,
            },
            Case {
                name: "user content before and after markers",
                input: format!(
                    "# Header\n\n{}\ngenerated\n{}\n\n# Footer\n",
                    MARKER_BEGIN, MARKER_END
                ),
                want: Some("# Header\n\n# Footer"),
            },
        ];

        for tc in cases {
            let got = crate::agentmd::extract_user_content(&tc.input);
            assert_eq!(got.as_deref(), tc.want, "case: {}", tc.name);
        }
    }

    #[test]
    fn agent_md_full_round_trip_through_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("templates");

        // Create a template with agent_md
        let tmpl = Template {
            repos: vec![TemplateRepo {
                url: "git@github.com:acme/api.git".into(),
            }],
            config: None,
            agent_md: Some("# Project Rules\n\nAlways use table-driven tests.".into()),
        };

        // Save and reload
        save(&dir, "with-agent", &tmpl).unwrap();
        let loaded = load(&dir, "with-agent").unwrap();

        assert_eq!(loaded.agent_md, tmpl.agent_md);
        assert_eq!(loaded.repos.len(), 1);
    }

    #[test]
    fn agent_md_with_markers_rejected() {
        use crate::agentmd::MARKER_BEGIN;

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("evil.yaml");
        let yaml = format!(
            "repos:\n  - url: git@github.com:acme/api.git\nagent_md: \"fake\\n{}\\nevil\"\n",
            MARKER_BEGIN
        );
        std::fs::write(&path, &yaml).unwrap();

        let err = load_from_file(&path).unwrap_err();
        assert!(
            err.to_string().contains("wsp markers"),
            "expected marker rejection, got: {}",
            err
        );
    }
}
