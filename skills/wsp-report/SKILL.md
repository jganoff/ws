---
name: wsp-report
description: Report a wsp issue on GitHub with full diagnostic context
user_invocable: true
---

# Report a wsp Issue

Gather diagnostic context and file a GitHub issue for the wsp tool.

## When to use

Use this skill when you encounter a bug, unexpected behavior, or error while using `wsp`. This skill collects everything needed for a useful bug report.

## Steps

### 1. Ask the user what went wrong

Before gathering diagnostics, ask the user to describe:
- What command they ran (or you ran) that failed
- What they expected to happen
- What actually happened
- The full error output (if not already visible in the conversation)

### 2. Gather diagnostic context

Run ALL of the following commands in parallel and capture the output:

```bash
wsp --version
uname -srm
echo $SHELL
wsp st --json 2>&1
wsp setup config ls --json 2>&1
wsp setup repo ls --json 2>&1
cat .wsp.yaml 2>/dev/null || echo "not in a workspace"
```

### 3. Sanitize before sharing

**Before formatting the issue, redact sensitive information:**
- Replace full filesystem paths (e.g., `/Users/jganoff/dev/...`) with relative paths or `~`
- Replace private repository URLs/identities with `<private-repo>`
- The `uname -srm` output is safe (no hostname). If you used `uname -a`, strip the hostname field
- Remove any tokens, passwords, or credentials if they appear in error output
- Review branch names — they may reveal internal project codenames

Ask the user to confirm the sanitized output before proceeding.

### 4. Reproduce (if possible)

If the failing command can be safely re-run, execute it again to capture fresh output. Include both stdout and stderr. If the command is destructive or has side effects, do NOT re-run it — use whatever output is already available from the conversation.

### 5. Format the issue

Create a GitHub issue with this structure:

```
Title: <type>: <concise description>
  - Types: bug, crash, unexpected-behavior

Body:
## Description
<1-3 sentences describing the problem>

## Steps to Reproduce
1. <exact commands>
2. ...

## Expected Behavior
<what should happen>

## Actual Behavior
<what actually happened, including error output>

## Environment
- wsp version: <output of wsp --version>
- OS: <output of uname -srm>
- Shell: <$SHELL>

## Workspace State
<output of wsp st --json, if relevant>

## Configuration
<output of wsp setup config ls --json>
```

### 6. File the issue

Write the issue body to a temp file and use `--body-file` to avoid shell quoting issues:

```bash
gh issue create --repo jganoff/wsp --title "bug: <description>" --body-file /tmp/wsp-issue-body.md
```

Or using a heredoc:

```bash
gh issue create --repo jganoff/wsp --title "bug: <description>" --body "$(cat <<'EOF'
<issue body here>
EOF
)"
```

**IMPORTANT:** Always show the user the formatted issue and get explicit confirmation before filing.

### 7. Report back

Share the issue URL with the user after filing. Clean up any temp files.

## Notes

- If `gh` CLI is not available, format the issue as markdown and ask the user to paste it at https://github.com/jganoff/wsp/issues/new
- Include conversation context if relevant — what the agent was trying to do when the error occurred
- If the error involves a specific repo, include the repo's origin remote URL only if it's a public repo
