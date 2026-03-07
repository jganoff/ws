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
wsp template new dash --from-workspace billing    # derive from an existing workspace
```

When `--from-workspace` is given, the template is populated from the workspace's current repo set (with URLs looked up from the registry) and applicable settings.

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

## Deferred Decisions

- **Template composition.** Whether a template can include/extend another template. Adds complexity — defer until there's a clear need.
- **Team bootstrap (`.wsp-team.yaml`).** A bundle of templates + global defaults for team onboarding. Templates are the building block; team bootstrap is a higher-level orchestration that imports multiple templates at once.
- **Agent context in templates.** Templates currently capture repos and (future) settings, but not the workspace-level AI agent context: AGENTS.md content, CLAUDE.md project instructions, or installed skills. A template that includes these would make `wsp new -t` produce a fully configured workspace for both humans and AI agents — "one file gets a teammate fully set up" including agent instructions, not just repo lists. Design questions: should templates embed the content inline or reference external files? How does this interact with the auto-generated marked sections in AGENTS.md? Should skills be versioned or fetched at creation time?
- **Repo-embedded templates.** A repo could ship a `.wsp-template.yaml` at its root, discovered automatically during `wsp registry add` or `wsp new`. This turns git itself into the distribution mechanism for workspace definitions — no need for a separate sharing/fetching layer. A repo like `api-gateway` could declare "I'm typically used with `user-service` and `proto`" and include agent context. Design questions: what triggers discovery (opt-in flag, auto-detect, prompt)? How does a repo-embedded template interact with stored templates? Does it auto-register the companion repos, or just suggest them? Should the embedded template live at a conventional path (`.wsp-template.yaml`) or be configurable?

## Implementation Phases

### Phase 1: Core template CRUD

- Template file schema and serde types
- `wsp template new/ls/show/rm` (including `--from-workspace`)
- `wsp new -t <name>` support
- Auto-register unknown repos during `wsp new -t`
- `created_from` field in `.wsp.yaml`

### Phase 2: Import / Export

- `wsp template export <name>`
- `wsp template import <file|url>`
- Mirror cloning for unknown repos during import

### Phase 3: Group migration

- Auto-migrate existing groups to templates
- Deprecation warning on `-g`
- Update docs and SKILL.md

### Phase 4: Settings

- Wire template settings into workspace creation (language integrations, sync strategy)
- Settings precedence logic
