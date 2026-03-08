# Feature: Workspace Templates

Sharable, self-contained artifacts that describe how to create a workspace — repos and settings.

## Motivation

Today, groups are named repo lists in global config. They carry no settings and aren't shareable. When a teammate asks "how do I set up the dash workspace?", the answer is a series of manual commands.

Workspace templates solve this by packaging everything needed to create a workspace into a single file that can be shared, version-controlled, or committed to a team repo.

## File Format

```yaml
# dash.wsp.yaml
repos:
  - url: https://github.com/docker/api-gateway.git
  - url: https://github.com/docker/user-service.git
  - url: https://github.com/docker/proto.git
config:
  language_integrations:
    go: true
  sync_strategy: rebase
```

### Design decisions

- **Explicit URLs.** Templates are shareable artifacts. A new person may not have repos registered, so URLs must be explicit — not shortnames or identities.
- **Identities are derived.** The identity (`github.com/docker/api-gateway`) is derived from the URL using `giturl::parse`, not stored separately.
- **Settings are optional.** A template with just `repos` is valid.
- **All repos are active.** Every repo in a template gets the workspace branch.
- **Stamp-and-go.** Once a workspace is created from a template, there is no live link. The workspace is independent. See [Relationship Model](#relationship-model) below.

## Storage

- Source of truth: `~/.local/share/wsp/templates/<name>.yaml`
- Shareable format: `<name>.wsp.yaml` (same schema, portable filename)

## CLI

### Management

```
wsp template new dash                     # interactive or from flags
wsp template ls                           # list templates
wsp template show dash                    # show template contents
wsp template edit dash                    # open in $EDITOR
wsp template rm dash                      # delete template
```

### Create from existing workspace

```
wsp template new dash                             # interactive or from flags
wsp template new dash -w billing                  # derive from an existing workspace
```

When `-w`/`--workspace` is given, the template is populated from the workspace's current repo set (with URLs looked up from the registry) and applicable settings.

### Import / Export

```
wsp template export dash                  # write dash.wsp.yaml to cwd
wsp template export dash --stdout         # print to stdout
wsp template import dash.wsp.yaml         # install to templates dir
wsp template import https://...           # fetch and install (HTTPS only)
```

Import registers any unknown repos (clones mirrors) automatically — one file gets a new teammate fully set up.

### Usage

```
wsp new my-feature -t dash                # create workspace from template
wsp new my-feature -t dash api-gateway    # template + extra inline repos
```

The `-t` / `--template` flag replaces `-g` / `--group`.

Inline repos can still be specified without a template:
```
wsp new my-feature api-gateway user-service
```

## Relationship Model

Templates use a **stamp-and-go** model. A template is a cookie cutter; the workspace is the cookie. Once created, the workspace is independent.

### What this means

- **No live link.** Changing a template does not affect existing workspaces.
- **No drift detection.** A workspace that diverges from its birth template (repos added/removed) is normal, not exceptional.
- **No cascade on delete.** Deleting a template has zero effect on workspaces created from it.
- **Ad-hoc workspaces are first-class.** A workspace created without a template has identical capabilities.

### Provenance metadata

When a workspace is created from a template, `.wsp.yaml` records the source:

```yaml
name: my-feature
branch: jganoff/my-feature
created_from: dash          # informational only, not a foreign key
repos:
  github.com/docker/api-gateway: null
  github.com/docker/user-service: null
```

This field is informational — `wsp ls` can display it, but no behavior depends on it. If the template is later deleted, the field is stale. That's fine.

### Why not a live link

- **Source of truth ambiguity.** If the template says [A, B, C] but the workspace has [A, B, D], which is correct? Neither — they answer different questions.
- **Merge semantics are undefined.** When a template adds repo E and the user manually added repo D, what does "sync from template" mean?
- **Violates "clones are the developer's space."** Design tenet 5 says the developer has full autonomy inside a clone. A live link extends this tension to the workspace level.
- **Fast recreation replaces reconciliation.** If a template evolves, the user creates a new workspace. Mirror-backed cloning makes this cheap.

## Config Precedence

```
workspace override > template config > global default
```

For example, if a template sets `language_integrations.go: true` but global config sets `go: false`, the template wins. If a future workspace-level override exists, it wins over both.

## Migration from Groups

Templates replace groups. Migration path:

1. Auto-migrate: each existing `GroupEntry` becomes a template file with just `repos` (no settings). URLs are looked up from registered repos.
2. `wsp new -g` continues to work as an alias for `-t` with a deprecation warning.
3. Remove `-g` in a future major version.

The `groups` key in `config.yaml` is preserved for backward compatibility during the transition but is read-only — all mutations go through templates.

## Format Unification (.wsp.yaml as template)

The original design had two formats: `.wsp.yaml` (workspace metadata with identities) and a separate template format (repo URLs + config). This creates friction — two schemas, conversion logic, and a separate `~/.local/share/wsp/templates/` directory.

### Decision: `.wsp.yaml` IS the template

`.wsp.yaml` gains an optional `url` field on each repo entry, making it self-contained and shareable:

```yaml
name: my-feature
branch: jganoff/my-feature
repos:
  github.com/acme/api-gateway:
    url: git@github.com:acme/api-gateway.git
  github.com/acme/user-service:
    url: git@github.com:acme/user-service.git
config:
  language_integrations:
    go: true
created: 2026-03-07T10:00:00Z
```

Any `.wsp.yaml` can be used as a template: `wsp new my-feature -t ./path/to/.wsp.yaml`. The `name` and `branch` fields are ignored (the new workspace gets its own). Only `repos` (with URLs) and `config` are used.

### Implications

- **No separate template format.** The `Template` struct and `~/.local/share/wsp/templates/` directory are retained as "saved workspace definitions" (named `.wsp.yaml` snapshots), but the underlying format converges.
- **URLs are snapshots.** The URL in `.wsp.yaml` captures what was used at creation time. The registry remains the source of truth for mirrors. Stale URLs are flagged by `wsp doctor`, not auto-synced.
- **Backward compatible.** The `url` field is optional (`#[serde(default)]`). Old `.wsp.yaml` files without URLs still work locally; they just aren't shareable as templates.
- **Workspace definition repos.** A git repo containing a `.wsp.yaml` + CLAUDE.md + skills/ becomes a shareable workspace definition. Git is the distribution mechanism — no URL fetching needed.

### Doctor checks for URL drift

`wsp doctor` flags workspaces where `.wsp.yaml` URLs differ from registry URLs. `--fix` updates them. This is a diagnostic, not an automatic sync.

## Deferred Decisions

- **Template composition.** Whether a template can include/extend another template. Adds complexity — defer until there's a clear need.
- **Team bootstrap (`.wsp-team.yaml`).** A bundle of templates + global defaults for team onboarding. Templates are the building block; team bootstrap is a higher-level orchestration that imports multiple templates at once.
- **Agent context.** Workspace definition repos could include CLAUDE.md, AGENTS.md, and skills/ alongside `.wsp.yaml`. When creating a workspace from such a repo, wsp copies the agent files into the new workspace. Design questions: how does this interact with auto-generated AGENTS.md marked sections? Should skills be versioned or fetched at creation time?
- **Repo-embedded workspace definitions.** A repo could ship a `.wsp.yaml` at its root declaring companion repos. Discovered during `wsp registry add` or `wsp new`. Design questions: what triggers discovery (opt-in, auto-detect, prompt)? Does it auto-register companions or just suggest them?

## Implementation Phases

All phases have shipped (0.8.0).
