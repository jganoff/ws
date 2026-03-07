use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::Paths;
use crate::giturl;
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
    let data = fs::read_to_string(&path).with_context(|| format!("reading template {:?}", name))?;
    let t: Template =
        serde_yaml_ng::from_str(&data).with_context(|| format!("parsing template {:?}", name))?;
    if t.repos.is_empty() {
        bail!("template {:?} has no repos", name);
    }
    Ok(t)
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

/// Create a template from an existing workspace's repo set.
pub fn from_workspace(paths: &Paths, ws_name: &str) -> Result<Template> {
    let ws_dir = workspace::dir(&paths.workspaces_dir, ws_name);
    let meta = workspace::load_metadata(&ws_dir)
        .with_context(|| format!("loading workspace {:?}", ws_name))?;
    let cfg = crate::config::Config::load_from(&paths.config_path)?;

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
}
