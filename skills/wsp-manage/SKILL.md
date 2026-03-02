---
name: wsp-manage
description: Manage multi-repo workspaces with wsp
user_invocable: true
---

# wsp — Multi-Repo Workspace Manager

Use `wsp` to manage workspaces that span multiple git repositories. Each workspace creates local clones from bare mirror clones, sharing a single branch name across repos.

**Always use `--json` when calling wsp programmatically.** JSON output goes to stdout; progress messages go to stderr.

## Quick Reference

### Repos (global registry)

```bash
wsp setup repo add <url>                        # Register and bare-clone a repository
wsp setup repo list                             # List registered repositories [read-only]
wsp setup repo remove <name>                    # Remove a repository and its mirror
```

### Groups (named sets of repos)

```bash
wsp setup group new <name> <repos>...           # Create a new repo group
wsp setup group list                            # List all groups [read-only]
wsp setup group show <name>                     # Show repos in a group [read-only]
wsp setup group delete <name>                   # Delete a group
wsp setup group update <name> [--add <add>]... [--remove <remove>]... # Add or remove repos from a group
```

### Workspaces

```bash
wsp new <workspace> [<repos>]... [-g <group>] [--no-fetch] # Create a new workspace
wsp ls                                          # List active workspaces [read-only] (alias: list)
wsp st [<workspace>] [-v]                       # Git status across workspace repos [read-only] (alias: status)
wsp diff [<workspace>] [<args>]...              # Show git diff across workspace repos [read-only]
wsp log [<workspace>] [--oneline] [<args>]...   # Show commits ahead of upstream per workspace repo [read-only]
wsp sync [<workspace>] [--strategy <strategy>] [--dry-run] # Fetch and rebase/merge all workspace repos
wsp exec <workspace> <command>...               # Run a command in each repo of a workspace
wsp cd <workspace>                              # Change directory into a workspace [read-only]
wsp rm [<workspace>] [-f]                       # Remove a workspace (alias: remove)
wsp repo add [<repos>]... [-g <group>]          # Add repos to current workspace
wsp repo rm <repos>... [-f]                     # Remove repo(s) from the current workspace (alias: remove)
wsp repo fetch [--all] [--prune]                # Fetch updates for workspace repos
wsp repo ls                                     # List repos in the current workspace [read-only] (alias: list)
```

### Config

```bash
wsp setup config list                           # List all config values [read-only]
wsp setup config get <key>                      # Get a config value [read-only]
wsp setup config set <key> <value>              # Set a config value
wsp setup config unset <key>                    # Unset a config value
```

### Skill management

```bash
wsp setup skill install                         # Install wsp Claude Code skill to ~/.claude/skills/
```

## JSON Output Schemas

### `wsp setup repo list --json`
```json
{
  "repos": [
    {
      "identity": "github.com/acme/api-gateway",
      "shortname": "api-gateway",
      "url": "git@github.com:acme/api-gateway.git"
    }
  ]
}
```

### `wsp ls --json`
```json
{
  "workspaces": [
    {
      "name": "my-feature",
      "branch": "my-feature",
      "repo_count": 2,
      "path": "~/dev/workspaces/my-feature"
    }
  ]
}
```

### `wsp st --json`
```json
{
  "workspace": "my-feature",
  "branch": "my-feature",
  "repos": [
    {
      "name": "api-gateway",
      "branch": "my-feature",
      "ahead": 2,
      "changed": 1,
      "has_upstream": true,
      "status": "2 ahead, 1 modified"
    }
  ]
}
```

### `wsp diff --json`
```json
{
  "repos": [
    {
      "name": "api-gateway",
      "diff": "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,4 @@\n+use std::io;\n ..."
    }
  ]
}
```

### `wsp log --json`
```json
{
  "repos": [
    {
      "name": "api-gateway",
      "commits": [
        {
          "hash": "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
          "timestamp": 1700000000,
          "subject": "feat: add billing endpoint"
        }
      ]
    }
  ]
}
```

### `wsp sync --json`
```json
{
  "workspace": "my-feature",
  "branch": "my-feature",
  "dry_run": false,
  "repos": [
    {
      "name": "api-gateway",
      "action": "rebase onto origin/main",
      "ok": true,
      "detail": "2 commit(s) rebased"
    }
  ]
}
```

### `wsp repo ls --json`
```json
{
  "repos": [
    {
      "identity": "github.com/acme/api-gateway",
      "shortname": "api-gateway",
      "dir_name": "api-gateway"
    },
    {
      "identity": "github.com/acme/shared-lib",
      "shortname": "shared-lib",
      "dir_name": "shared-lib",
      "git_ref": "main"
    }
  ]
}
```

### `wsp repo fetch --json`
```json
{
  "repos": [
    {
      "identity": "github.com/acme/api-gateway",
      "shortname": "api-gateway",
      "ok": true
    }
  ]
}
```

### `wsp setup group list --json`
```json
{
  "groups": [
    {
      "name": "backend",
      "repo_count": 3
    }
  ]
}
```

### `wsp setup group show <name> --json`
```json
{
  "name": "backend",
  "repos": [
    "api-gateway",
    "user-service",
    "shared-lib"
  ]
}
```

### `wsp setup config list --json`
```json
{
  "entries": [
    {
      "key": "branch-prefix",
      "value": "jg"
    },
    {
      "key": "workspaces-dir",
      "value": "~/dev/workspaces"
    },
    {
      "key": "sync-strategy",
      "value": "rebase"
    }
  ]
}
```

### `wsp setup config get <key> --json`
```json
{
  "key": "branch-prefix",
  "value": "jg"
}
```

### `Mutation commands (new, rm, add, remove, set, etc.)`
```json
{
  "ok": true,
  "message": "Registered github.com/acme/api-gateway"
}
```

### `Errors`
```json
{
  "error": "repo \"foo\" not found"
}
```

## Shortname Resolution

Repos are identified by `host/owner/repo` (e.g., `github.com/acme/api-gateway`). You can use the shortest unique suffix:
- `api-gateway` if unambiguous
- `acme/api-gateway` to disambiguate from `other-org/api-gateway`

## `@ref` Syntax for Context Repos

When creating a workspace, pin a repo to a specific branch/tag/SHA:
```bash
wsp new my-feature api-gateway user-service@main proto@v1.0
```
- `api-gateway` — active repo, gets the workspace branch
- `user-service@main` — context repo, checked out at `main`
- `proto@v1.0` — context repo, checked out at tag `v1.0`

## Directory Layout

```
~/dev/workspaces/<workspace-name>/
  .wsp.yaml              # Workspace metadata
  <repo-name>/          # Local clone for each repo
```

## Common Agent Workflows

### Create a workspace and start working
```bash
wsp setup repo list --json                     # See available repos
wsp new my-feature api-gateway user-service    # Create workspace
cd ~/dev/workspaces/my-feature                # Enter workspace
```

### Check what's changed
```bash
wsp st --json          # From inside a workspace
wsp diff --json        # See all diffs
```

### Sync with upstream
```bash
wsp sync --json        # Fetch + rebase all repos
wsp sync --strategy merge --json  # Use merge instead of rebase
```

### Run tests across all repos
```bash
wsp exec my-feature -- make test
```

### Clean up when done
```bash
wsp rm my-feature      # Removes clones + branch (if merged)
wsp rm my-feature -f   # Force remove even if unmerged
```
