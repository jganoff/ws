# Feature: Workspace Definitions

Sharable, self-contained artifacts that describe how to create a workspace — repos, context repos, and settings.

## Motivation

Today, groups are named repo lists in global config. They carry no settings, no context repo declarations, and aren't shareable. When a teammate asks "how do I set up the dash workspace?", the answer is a series of manual commands.

Workspace definitions solve this by packaging everything needed to create a workspace into a single file that can be shared, version-controlled, or committed to a team repo.

## File Format

```yaml
# dash.wsp.yaml
repos:
  - url: https://github.com/docker/api-gateway.git
  - url: https://github.com/docker/user-service.git
context:
  - url: https://github.com/docker/proto.git
    ref: main
settings:
  language_integrations:
    go: true
  sync_strategy: rebase
```

### Design decisions

- **Explicit URLs.** Definitions are shareable artifacts. A new person may not have repos registered, so URLs must be explicit — not shortnames or identities.
- **Identities are derived.** The identity (`github.com/docker/api-gateway`) is derived from the URL using `giturl::parse`, not stored separately.
- **Settings are optional.** A definition with just `repos` is valid.
- **Context repos declare their ref.** Active repos don't — they get the workspace branch.

## Storage

- Source of truth: `~/.local/share/wsp/definitions/<name>.yaml`
- Shareable format: `<name>.wsp.yaml` (same schema, portable filename)

## CLI

### Management

```
wsp setup def new dash                     # interactive or from flags
wsp setup def ls                           # list definitions
wsp setup def show dash                    # show definition contents
wsp setup def edit dash                    # open in $EDITOR
wsp setup def rm dash                      # delete definition
```

### Import / Export

```
wsp setup def export dash                  # write dash.wsp.yaml to cwd
wsp setup def export dash --stdout         # print to stdout
wsp setup def import dash.wsp.yaml        # install to definitions dir
wsp setup def import https://...           # fetch and install (HTTPS only)
```

Import registers any unknown repos (clones mirrors) automatically — one file gets a new teammate fully set up.

### Usage

```
wsp new my-feature -d dash                 # create workspace from definition
wsp new my-feature -d dash api-gateway     # definition + extra inline repos
```

The `-d` / `--def` flag replaces `-g` / `--group`.

Inline repos can still be specified without a definition:
```
wsp new my-feature api-gateway user-service
```

## Settings Precedence

```
workspace override > definition setting > global default
```

For example, if a definition sets `language_integrations.go: true` but global config sets `go: false`, the definition wins. If a future workspace-level override exists, it wins over both.

## Migration from Groups

Definitions replace groups. Migration path:

1. Auto-migrate: each existing `GroupEntry` becomes a definition file with just `repos` (no settings, no context). URLs are looked up from registered repos.
2. `wsp new -g` continues to work as an alias for `-d` with a deprecation warning.
3. Remove `-g` in a future major version.

The `groups` key in `config.yaml` is preserved for backward compatibility during the transition but is read-only — all mutations go through definitions.

## Deferred Decisions

- **Live link.** Whether workspaces maintain a reference to their source definition for re-applying settings (e.g., when the definition evolves). For now, definitions are stamp-and-go templates.
- **Definition composition.** Whether a definition can include/extend another definition. Adds complexity — defer until there's a clear need.
- **Team bootstrap (`.wsp-team.yaml`).** A bundle of definitions + global defaults for team onboarding. Definitions are the building block; team bootstrap is a higher-level orchestration that imports multiple definitions at once.

## Implementation Phases

### Phase 1: Core definition CRUD

- Definition file schema and serde types
- `wsp setup def new/ls/show/rm`
- `wsp new -d <name>` support
- Auto-register unknown repos during `wsp new -d`

### Phase 2: Import / Export

- `wsp setup def export <name>`
- `wsp setup def import <file|url>`
- Mirror cloning for unknown repos during import

### Phase 3: Group migration

- Auto-migrate existing groups to definitions
- Deprecation warning on `-g`
- Update docs and SKILL.md

### Phase 4: Settings

- Wire definition settings into workspace creation (language integrations, sync strategy)
- Settings precedence logic
