---
name: wsp-new-feature
description: Create a new wsp workspace for a feature
user_invocable: true
---

# Create a New Feature Workspace

Create a new wsp workspace for working on a feature across one or more repositories.

## Arguments

- `<name>` (required) — the workspace name (typically a feature branch name like `jganoff/my-feature`)

## Steps

### 1. Determine the template

Check if the current workspace was created from a template:

```bash
wsp st --json
```

Look at the `created_from` field in the JSON output. If it has a value, offer to use the same template. Otherwise, list available templates:

```bash
wsp template ls --json
```

Present the templates to the user and let them pick one. If no template fits, ask the user which repos to include.

### 2. Create the workspace

If using a template:

```bash
wsp new <name> -t <template> --json
```

If using ad-hoc repos:

```bash
wsp new <name> <repo1> <repo2> ... --json
```

### 3. Report the result

Parse the JSON output and report:
- The new workspace path
- Which repos were cloned
- The branch name

Tell the user they can `cd` into the workspace path to start working, or use `wsp cd <name>` if they have the shell integration set up.

## Notes

- **Always use `--json`** when calling wsp programmatically. JSON output goes to stdout; progress messages go to stderr.
- If `wsp new` fails because mirrors are missing, suggest running `wsp registry add <repo>` first to set up the bare clones.
- The workspace name becomes the git branch name across all repos — keep it short and descriptive.
