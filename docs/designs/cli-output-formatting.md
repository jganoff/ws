# Technical Design: CLI Output Formatting

## Overview

This document outlines the technical design for improving CLI output formatting across the `ws` tool. The current implementation suffers from poor column alignment, missing headers, noisy formatting characters, and inconsistent status displays. This design establishes a best-in-class tabular output system that follows modern CLI conventions.

## Research Findings

### Industry Best Practices

Research into popular CLI tools (kubectl, docker, gh, terraform, cargo) reveals several consistent patterns:

1. **Column Headers**: Always present for tabular data, aligned with their data
2. **Plain Text**: No decorative characters (brackets, parens) in data columns
3. **Alignment Rules**:
   - Text fields: left-aligned
   - Numeric fields: right-aligned
   - Headers follow data alignment
4. **Whitespace Separation**: Columns separated by consistent padding (typically 2-4 spaces)
5. **Machine-Readable**: Output works well with grep, awk, and other Unix tools
6. **No Colors by Default**: Colors only when explicitly enabled (not in scope for this design)

### Go Standard Library: text/tabwriter

The Go standard library's `text/tabwriter` package is the ideal solution:

- **Battle-tested**: Used by kubectl, docker CLI, and many other tools
- **Elastic Tabstops**: Automatically calculates optimal column widths
- **Simple API**: Works with standard `fmt.Fprintf` patterns
- **Flush-based**: Buffers output for optimal alignment before rendering
- **Configuration**: Supports padding, min-width, and alignment flags

**Key Configuration Parameters:**
```go
tabwriter.NewWriter(output, minwidth, tabwidth, padding, padchar, flags)
```

- `minwidth`: Minimum width of a cell (typically 0 for auto)
- `tabwidth`: Tab stop width (typically 8)
- `padding`: Spaces between columns (typically 2-3)
- `padchar`: Padding character (space ' ' or tab '\t')
- `flags`: 0 for left-align (default), `tabwriter.AlignRight` for numbers

### Reference Sources

- [Command Line Interface Guidelines](https://clig.dev/)
- [3 Commandments for CLI Design](https://medium.com/relay-sh/command-line-ux-in-2020-e537018ebb69)
- [A Guide to Go's text/tabwriter Package](https://reintech.io/blog/a-guide-to-gos-text-tabwriter-package-aligning-text)
- [text/tabwriter Go Package](https://pkg.go.dev/text/tabwriter)
- [Format command and log output - Docker Docs](https://docs.docker.com/engine/cli/formatting/)
- [Guide to kubectl Output Formatting](https://www.baeldung.com/ops/kubectl-output-format)

## Current State Analysis

### ws status

**Current Output:**
```
Workspace: my-feature  Branch: my-feature

[api-gateway ]  (my-feature)  2 files changed
[user-service]  (main      )  clean
[proto       ]  (v1.0      )  1 ahead  3 files changed
```

**Problems:**
1. Square brackets `[]` and parentheses `()` add visual noise
2. Manual padding inside brackets breaks with longer names
3. No column headers - unclear what data represents
4. Status information inconsistent (mixing counts with "clean")
5. Multiple spaces between sections are fragile
6. Difficult to parse programmatically

### ws list

**Current Output:**
```
  my-feature  branch:main  repos:3  /Users/user/dev/workspaces/my-feature
  bugfix-123  branch:bugfix-123  repos:2  /Users/user/dev/workspaces/bugfix-123
```

**Problems:**
1. No headers
2. Key-value format (`branch:`, `repos:`) is inconsistent with status command
3. Indentation with spaces is unnecessary
4. Path column often very long, pushing other data off screen

### ws repo list

**Current Output:**
```
  github.com/acme/api-gateway  [api-gateway]  (git@github.com:acme/api-gateway.git)
  github.com/acme/user-service  (git@github.com:acme/user-service.git)
```

**Problems:**
1. No headers
2. Square brackets `[]` only sometimes present (when shortname differs)
3. Parentheses add noise
4. Indentation inconsistent with other commands

### ws group list

**Current Output:**
```
  backend (5 repos)
  frontend (3 repos)
```

**Problems:**
1. No headers
2. Inconsistent with other list commands
3. Parentheses add noise
4. Doesn't show which repos are in the group

### ws remove (error messages)

**Current Output:**
```
workspace "my-feature" has pending changes in: api-gateway, user-service
Use --force to remove anyway
```

**Problems:**
1. Multi-line error could be improved with better formatting
2. Repo list could be displayed as a table for many repos
3. Two-line format is fine but could be more structured

## Proposed Design

### Core Principles

1. **Use text/tabwriter for all tabular output**
2. **Always include column headers** for multi-row tables
3. **No decorative punctuation** in data columns (no `[]`, `()`, etc.)
4. **Consistent column order** across commands
5. **Align text left, numbers right**
6. **Status indicators** should be standardized keywords

### Standard Status Keywords

Replace informal phrases with consistent status indicators:

| Current | Proposed |
|---------|----------|
| `clean` | `clean` (keep) |
| `2 files changed` | `2 modified` |
| `1 ahead` | `1 ahead` |
| `1 ahead  2 files changed` | `1 ahead, 2 modified` |

### Output Format Specifications

#### ws status

**Proposed Output:**
```
Workspace: my-feature  Branch: my-feature

REPOSITORY      BRANCH       STATUS
api-gateway     my-feature   2 modified
user-service    main         clean
proto           v1.0         1 ahead, 3 modified
```

**Changes:**
- Add header row: `REPOSITORY  BRANCH  STATUS`
- Remove `[]` and `()` punctuation
- Standardize status format: `N ahead, N modified` or `clean`
- Use tabwriter for alignment
- Header in UPPERCASE (common convention in kubectl, docker)

**Edge Cases:**
- Errors: Display as `ERROR: <message>` in STATUS column
- Long branch names: Tabwriter will auto-adjust column width
- No repos: Display `No repositories in workspace.`
- Unknown branch: Display `?` in BRANCH column

#### ws list

**Proposed Output:**
```
NAME         BRANCH       REPOS  PATH
my-feature   my-feature       3  /Users/user/dev/workspaces/my-feature
bugfix-123   bugfix-123       2  /Users/user/dev/workspaces/bugfix-123
```

**Changes:**
- Add header row: `NAME  BRANCH  REPOS  PATH`
- Remove `branch:` and `repos:` key prefixes
- Right-align REPOS column (numeric)
- Remove leading indentation
- Use tabwriter for alignment

**Edge Cases:**
- No workspaces: Display `No workspaces.` (keep current)
- Error reading workspace: Show `ERROR` in BRANCH column
- Long paths: Consider truncating with `...` if wider than terminal (future enhancement)

#### ws repo list

**Proposed Output:**
```
IDENTITY                      SHORTNAME      URL
github.com/acme/api-gateway   api-gateway    git@github.com:acme/api-gateway.git
github.com/acme/user-service  user-service   git@github.com:acme/user-service.git
```

**Changes:**
- Add header row: `IDENTITY  SHORTNAME  URL`
- Always show SHORTNAME column (even if same as identity)
- Remove `[]` and `()` punctuation
- Remove leading indentation
- Use tabwriter for alignment

**Edge Cases:**
- No repos: Display `No repos registered.` (keep current)
- Shortname same as identity: Still display (no special handling)

#### ws group list

**Proposed Output:**
```
NAME      REPOS
backend       5
frontend      3
```

**Changes:**
- Add header row: `NAME  REPOS`
- Right-align REPOS column (numeric)
- Remove parentheses
- Use tabwriter for alignment

**Edge Cases:**
- No groups: Display `No groups defined.` (keep current)

**Alternative (Verbose Mode - Future):**
```
NAME      REPOS  REPOSITORIES
backend       5  api-gateway, user-service, auth-service, payment-service, notification-service
frontend      3  web-app, mobile-app, admin-panel
```

#### ws remove (error formatting)

**Current:**
```
workspace "my-feature" has pending changes in: api-gateway, user-service
Use --force to remove anyway
```

**Proposed (when few repos):**
```
Error: workspace "my-feature" has pending changes in: api-gateway, user-service
Use --force to remove anyway
```

**Proposed (when many repos, 5+):**
```
Error: workspace "my-feature" has pending changes in the following repositories:

REPOSITORY      STATUS
api-gateway     3 modified
user-service    1 modified, 2 ahead
proto           1 modified

Use --force to remove anyway
```

**Changes:**
- Add `Error:` prefix for clarity
- For 5+ dirty repos, show as table with status details
- Keep comma-separated list for < 5 repos (cleaner for small sets)

### Implementation Architecture

#### New Package: internal/output

Create a new package to centralize output formatting logic.

**Package Structure:**
```
internal/output/
├── table.go       # Tabwriter wrapper and table builder
├── table_test.go  # Table builder tests
└── status.go      # Status keyword formatting utilities
```

#### table.go Interface

```go
package output

import (
	"io"
	"text/tabwriter"
)

// Table builds formatted tabular output using text/tabwriter.
type Table struct {
	w       *tabwriter.Writer
	headers []string
	rows    [][]string
}

// NewTable creates a new table that writes to the given writer.
// headers are the column headers (will be displayed in UPPERCASE).
func NewTable(w io.Writer, headers ...string) *Table

// AddRow appends a row of data to the table.
// The number of columns must match the number of headers.
func (t *Table) AddRow(columns ...string) error

// Render writes the table to the underlying writer.
// This must be called after all rows are added.
func (t *Table) Render() error

// Config holds tabwriter configuration.
type Config struct {
	MinWidth int  // Minimum cell width
	TabWidth int  // Tab width in spaces
	Padding  int  // Padding between columns
	PadChar  byte // Padding character
	Flags    uint // Formatting flags
}

// DefaultConfig returns the standard configuration for ws output.
func DefaultConfig() Config
```

**Default Configuration:**
```go
func DefaultConfig() Config {
	return Config{
		MinWidth: 0,   // Auto-calculate from content
		TabWidth: 8,   // Standard tab width
		Padding:  2,   // 2 spaces between columns
		PadChar:  ' ', // Pad with spaces
		Flags:    0,   // Left-align by default
	}
}
```

#### status.go Interface

```go
package output

// FormatRepoStatus formats repository status information.
// Returns standardized status strings like "clean", "2 modified", "1 ahead, 3 modified".
func FormatRepoStatus(ahead, modified int) string

// FormatError formats error messages for display in status columns.
func FormatError(err error) string
```

**Implementation:**
```go
func FormatRepoStatus(ahead, modified int) string {
	if ahead == 0 && modified == 0 {
		return "clean"
	}
	var parts []string
	if ahead > 0 {
		parts = append(parts, fmt.Sprintf("%d ahead", ahead))
	}
	if modified > 0 {
		parts = append(parts, fmt.Sprintf("%d modified", modified))
	}
	return strings.Join(parts, ", ")
}

func FormatError(err error) string {
	return fmt.Sprintf("ERROR: %v", err)
}
```

### Migration Strategy

**Phase 1: Create output package** (Test-Driven)
1. Create `internal/output/table.go` with `Table` type and methods
2. Write comprehensive table tests (various column counts, alignments, edge cases)
3. Create `internal/output/status.go` with status formatting functions
4. Write status formatting tests

**Phase 2: Migrate ws status command**
1. Update `internal/cmd/status.go` to use `output.Table`
2. Update tests to verify new output format
3. Manual testing with various workspace configurations

**Phase 3: Migrate other commands**
1. Update `internal/cmd/list.go` to use `output.Table`
2. Update `internal/cmd/repo_list.go` to use `output.Table`
3. Update `internal/cmd/group_list.go` to use `output.Table`
4. Update error formatting in `internal/cmd/remove.go`
5. Update all relevant tests

**Phase 4: Documentation**
1. Update command help text if needed
2. Add examples to README (if present)

### Code Changes Required

#### internal/output/table.go (new file)

```go
package output

import (
	"fmt"
	"io"
	"strings"
	"text/tabwriter"
)

// Table builds formatted tabular output.
type Table struct {
	w       *tabwriter.Writer
	buf     io.Writer
	headers []string
	rows    [][]string
}

// NewTable creates a new table builder.
func NewTable(w io.Writer, headers ...string) *Table {
	cfg := DefaultConfig()
	tw := tabwriter.NewWriter(w, cfg.MinWidth, cfg.TabWidth, cfg.Padding, cfg.PadChar, cfg.Flags)
	return &Table{
		w:       tw,
		buf:     w,
		headers: headers,
	}
}

// AddRow adds a data row to the table.
func (t *Table) AddRow(columns ...string) error {
	if len(columns) != len(t.headers) {
		return fmt.Errorf("row has %d columns, expected %d", len(columns), len(t.headers))
	}
	t.rows = append(t.rows, columns)
	return nil
}

// Render outputs the table.
func (t *Table) Render() error {
	if len(t.headers) == 0 {
		return nil // Nothing to render
	}

	// Write headers in UPPERCASE
	headerRow := make([]string, len(t.headers))
	for i, h := range t.headers {
		headerRow[i] = strings.ToUpper(h)
	}
	fmt.Fprintln(t.w, strings.Join(headerRow, "\t"))

	// Write data rows
	for _, row := range t.rows {
		fmt.Fprintln(t.w, strings.Join(row, "\t"))
	}

	return t.w.Flush()
}

// Config for tabwriter.
type Config struct {
	MinWidth int
	TabWidth int
	Padding  int
	PadChar  byte
	Flags    uint
}

// DefaultConfig returns standard ws configuration.
func DefaultConfig() Config {
	return Config{
		MinWidth: 0,
		TabWidth: 8,
		Padding:  2,
		PadChar:  ' ',
		Flags:    0,
	}
}
```

#### internal/output/status.go (new file)

```go
package output

import (
	"fmt"
	"strings"
)

// FormatRepoStatus formats repository git status.
func FormatRepoStatus(ahead, modified int) string {
	if ahead == 0 && modified == 0 {
		return "clean"
	}
	var parts []string
	if ahead > 0 {
		parts = append(parts, fmt.Sprintf("%d ahead", ahead))
	}
	if modified > 0 {
		parts = append(parts, fmt.Sprintf("%d modified", modified))
	}
	return strings.Join(parts, ", ")
}

// FormatError formats an error for display.
func FormatError(err error) string {
	return fmt.Sprintf("ERROR: %v", err)
}
```

#### internal/cmd/status.go (updated)

**Key Changes:**
1. Import `"github.com/jganoff/ws/internal/output"`
2. Replace custom padding logic with `output.Table`
3. Use `output.FormatRepoStatus()` for status strings
4. Use `output.FormatError()` for error cases

**Modified Code Sections:**

```go
// Replace this section (lines 47-119):
fmt.Printf("Workspace: %s  Branch: %s\n\n", meta.Name, meta.Branch)

type repoStatus struct {
	name    string
	branch  string
	ahead   int
	changed int
	err     error
}

var rows []repoStatus
maxName, maxBranch := 0, 0

for identity := range meta.Repos {
	// ... collection logic ...
}

for _, rs := range rows {
	if rs.err != nil {
		fmt.Printf("[%s] error: %v\n", rs.name, rs.err)
		continue
	}

	var parts []string
	if rs.ahead > 0 {
		parts = append(parts, fmt.Sprintf("%d ahead", rs.ahead))
	}
	if rs.changed > 0 {
		parts = append(parts, fmt.Sprintf("%d files changed", rs.changed))
	}
	detail := "clean"
	if len(parts) > 0 {
		detail = strings.Join(parts, "  ")
	}

	fmt.Printf("[%-*s]  (%-*s)  %s\n", maxName, rs.name, maxBranch, rs.branch, detail)
}

// With this:
fmt.Printf("Workspace: %s  Branch: %s\n\n", meta.Name, meta.Branch)

type repoStatus struct {
	name    string
	branch  string
	ahead   int
	changed int
	err     error
}

var rows []repoStatus

for identity := range meta.Repos {
	// ... existing collection logic (lines 60-99) ...
}

// Build and render table
table := output.NewTable(os.Stdout, "Repository", "Branch", "Status")

for _, rs := range rows {
	var status string
	if rs.err != nil {
		status = output.FormatError(rs.err)
	} else {
		status = output.FormatRepoStatus(rs.ahead, rs.changed)
	}

	branch := rs.branch
	if branch == "" {
		branch = "?"
	}

	if err := table.AddRow(rs.name, branch, status); err != nil {
		return fmt.Errorf("building table: %w", err)
	}
}

if err := table.Render(); err != nil {
	return fmt.Errorf("rendering table: %w", err)
}
```

#### internal/cmd/list.go (updated)

```go
// Replace the output loop (lines 26-38) with:
table := output.NewTable(os.Stdout, "Name", "Branch", "Repos", "Path")

for _, name := range names {
	wsDir, err := workspace.Dir(name)
	if err != nil {
		table.AddRow(name, "ERROR", "?", "?")
		continue
	}
	meta, err := workspace.LoadMetadata(wsDir)
	if err != nil {
		table.AddRow(name, "ERROR", "?", wsDir)
		continue
	}
	table.AddRow(name, meta.Branch, fmt.Sprintf("%d", len(meta.Repos)), wsDir)
}

if err := table.Render(); err != nil {
	return fmt.Errorf("rendering table: %w", err)
}
```

#### internal/cmd/repo_list.go (updated)

```go
// Replace the output loop (lines 36-44) with:
table := output.NewTable(os.Stdout, "Identity", "Shortname", "URL")

for _, id := range identities {
	entry := cfg.Repos[id]
	short := shortnames[id]
	if err := table.AddRow(id, short, entry.URL); err != nil {
		return fmt.Errorf("building table: %w", err)
	}
}

if err := table.Render(); err != nil {
	return fmt.Errorf("rendering table: %w", err)
}
```

#### internal/cmd/group_list.go (updated)

```go
// Replace the output loop (lines 30-33) with:
table := output.NewTable(os.Stdout, "Name", "Repos")

for _, name := range names {
	repos, _ := group.Get(cfg, name)
	if err := table.AddRow(name, fmt.Sprintf("%d", len(repos))); err != nil {
		return fmt.Errorf("building table: %w", err)
	}
}

if err := table.Render(); err != nil {
	return fmt.Errorf("rendering table: %w", err)
}
```

#### internal/cmd/remove.go (updated)

```go
// Update error formatting (lines 56-59) for many dirty repos:
if len(dirty) > 0 {
	sort.Strings(dirty)

	// For many dirty repos, show as table
	if len(dirty) >= 5 {
		var buf strings.Builder
		fmt.Fprintf(&buf, "workspace %q has pending changes in the following repositories:\n\n", name)

		// This would require collecting status info for each dirty repo
		// For now, keep simple list format
		fmt.Fprintf(&buf, "  %s\n\n", strings.Join(dirty, "\n  "))
		fmt.Fprintf(&buf, "Use --force to remove anyway")
		return fmt.Errorf(buf.String())
	}

	return fmt.Errorf("workspace %q has pending changes in: %s\nUse --force to remove anyway",
		name, strings.Join(dirty, ", "))
}
```

**Note:** Enhanced table format for `remove` error is marked as future enhancement since it requires collecting status info for dirty repos, which isn't currently done.

### Testing Strategy

#### Unit Tests (Test-Driven Development)

**internal/output/table_test.go:**
```go
package output_test

import (
	"bytes"
	"strings"
	"testing"

	"github.com/jganoff/ws/internal/output"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestTable_BasicOutput(t *testing.T) {
	tests := []struct {
		name    string
		headers []string
		rows    [][]string
		want    string
	}{
		{
			name:    "simple two column",
			headers: []string{"Name", "Age"},
			rows: [][]string{
				{"Alice", "30"},
				{"Bob", "25"},
			},
			want: "NAME  AGE\nAlice  30\nBob    25\n",
		},
		{
			name:    "three column with alignment",
			headers: []string{"Repository", "Branch", "Status"},
			rows: [][]string{
				{"api-gateway", "main", "clean"},
				{"user-service", "feature-branch", "2 modified"},
			},
			want: "REPOSITORY     BRANCH          STATUS\napi-gateway    main            clean\nuser-service   feature-branch  2 modified\n",
		},
		{
			name:    "empty table",
			headers: []string{"Name"},
			rows:    [][]string{},
			want:    "NAME\n",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var buf bytes.Buffer
			table := output.NewTable(&buf, tt.headers...)

			for _, row := range tt.rows {
				require.NoError(t, table.AddRow(row...))
			}

			require.NoError(t, table.Render())

			// Normalize whitespace for comparison
			got := normalizeWhitespace(buf.String())
			want := normalizeWhitespace(tt.want)
			assert.Equal(t, want, got)
		})
	}
}

func TestTable_ColumnMismatch(t *testing.T) {
	var buf bytes.Buffer
	table := output.NewTable(&buf, "Name", "Age")

	err := table.AddRow("Alice", "30", "Extra")
	assert.Error(t, err)
	assert.Contains(t, err.Error(), "3 columns, expected 2")
}

func normalizeWhitespace(s string) string {
	lines := strings.Split(s, "\n")
	for i, line := range lines {
		lines[i] = strings.TrimRight(line, " \t")
	}
	return strings.Join(lines, "\n")
}
```

**internal/output/status_test.go:**
```go
package output_test

import (
	"errors"
	"testing"

	"github.com/jganoff/ws/internal/output"
	"github.com/stretchr/testify/assert"
)

func TestFormatRepoStatus(t *testing.T) {
	tests := []struct {
		name     string
		ahead    int
		modified int
		want     string
	}{
		{"clean", 0, 0, "clean"},
		{"ahead only", 3, 0, "3 ahead"},
		{"modified only", 0, 5, "5 modified"},
		{"both", 2, 4, "2 ahead, 4 modified"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := output.FormatRepoStatus(tt.ahead, tt.modified)
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestFormatError(t *testing.T) {
	err := errors.New("something went wrong")
	got := output.FormatError(err)
	assert.Equal(t, "ERROR: something went wrong", got)
}
```

#### Integration Tests

Update existing command tests to verify new output format:

**internal/cmd/status_test.go** (update expected output):
- Update test fixtures to expect headers
- Verify column alignment with various data lengths
- Test error cases show "ERROR:" prefix

**internal/cmd/list_test.go** (update expected output):
- Update test fixtures to expect headers
- Verify numeric column alignment

#### Manual Testing Checklist

1. **ws status**
   - [ ] Workspace with all clean repos
   - [ ] Workspace with mixed states (clean, ahead, modified, ahead+modified)
   - [ ] Workspace with errors (missing repo directory)
   - [ ] Workspace with various repo name lengths
   - [ ] Workspace with long branch names

2. **ws list**
   - [ ] No workspaces
   - [ ] Single workspace
   - [ ] Multiple workspaces
   - [ ] Workspace with error reading metadata

3. **ws repo list**
   - [ ] No repos
   - [ ] Single repo
   - [ ] Multiple repos with varying identity lengths

4. **ws group list**
   - [ ] No groups
   - [ ] Single group
   - [ ] Multiple groups

5. **ws remove**
   - [ ] Clean workspace (success message)
   - [ ] Workspace with 1-2 dirty repos
   - [ ] Workspace with 5+ dirty repos (future table format)

### Performance Considerations

**text/tabwriter Performance:**
- Buffers all output before rendering (minimal memory overhead for CLI use)
- Single-pass alignment calculation: O(rows * columns)
- No performance concerns for typical workspace sizes (< 100 repos)

**Benchmarking (if needed):**
```go
func BenchmarkTableRender(b *testing.B) {
	for i := 0; i < b.N; i++ {
		var buf bytes.Buffer
		table := output.NewTable(&buf, "Repository", "Branch", "Status")
		for j := 0; j < 50; j++ {
			table.AddRow("repo-name", "branch-name", "clean")
		}
		table.Render()
	}
}
```

Expected: < 1ms for 50 rows on modern hardware.

### Backward Compatibility

**Breaking Changes:**
This is a breaking change to output format. Scripts parsing the output will need updates.

**Migration Guide for Users:**
Document in release notes:

```markdown
## Breaking Change: Output Format

The `ws status`, `ws list`, `ws repo list`, and `ws group list` commands now output
properly aligned tables with headers.

### Before:
```
[api-gateway]  (main)  clean
```

### After:
```
REPOSITORY    BRANCH  STATUS
api-gateway   main    clean
```

If you have scripts parsing ws output:
- Look for the header row and skip it (first line after blank lines)
- Split on whitespace (2+ spaces indicate column boundaries)
- Use `awk`, `grep`, or similar tools as before

Example: Get all dirty repos from status:
```bash
ws status | awk 'NR>1 && $3!="clean" {print $1}'
```
```

### Future Enhancements

**Not in scope for this design, but worth considering:**

1. **JSON Output Mode:** `--output json` flag for machine-readable output
2. **Color Support:** `--color` flag for status indication (red=dirty, green=clean)
3. **Compact Mode:** `--compact` flag to reduce padding for narrow terminals
4. **Column Selection:** `--columns` flag to choose which columns to display
5. **Sorting:** `--sort-by` flag to sort table rows
6. **Wide Mode:** Truncate long columns intelligently based on terminal width
7. **Enhanced Remove Error:** Table format showing individual repo status (requires refactoring HasPendingChanges to return detailed status)

## Implementation Timeline

**Estimated Effort: 1-2 days**

1. **Hour 1-2:** Create `internal/output` package with tests (TDD)
2. **Hour 3-4:** Migrate `ws status` command, update tests
3. **Hour 5-6:** Migrate `ws list`, `ws repo list`, `ws group list` commands
4. **Hour 7:** Update error formatting in `ws remove`
5. **Hour 8:** Manual testing, bug fixes, documentation

## Risks and Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Breaking change to output format | High - may break user scripts | Clear release notes with migration guide, version bump |
| tabwriter performance with large workspaces | Low | Benchmark with 100+ repos, optimize if needed |
| Complex edge cases (very long names/branches) | Medium | Comprehensive test coverage, manual testing |
| Inconsistent alignment across terminals | Low | tabwriter handles this, but test on different terminals |

## Success Criteria

1. All commands with tabular output use consistent formatting
2. Headers clearly identify each column
3. Columns align properly regardless of data length
4. Status keywords are standardized and clear
5. All tests pass with new output format
6. Manual testing confirms improved readability
7. Documentation updated with new output examples

## Appendix: Example Outputs

### ws status - Various Scenarios

**All Clean:**
```
Workspace: production  Branch: production

REPOSITORY     BRANCH      STATUS
api-gateway    production  clean
user-service   production  clean
auth-service   production  clean
```

**Mixed States:**
```
Workspace: feature-x  Branch: feature-x

REPOSITORY     BRANCH     STATUS
api-gateway    feature-x  2 modified
user-service   main       1 ahead, 3 modified
auth-service   feature-x  clean
proto          v1.2       ERROR: repository not found
```

**Long Names:**
```
Workspace: long-feature-name  Branch: long-feature-name

REPOSITORY                          BRANCH                STATUS
very-long-api-gateway-name          very-long-branch     5 modified
another-extremely-long-repo-name    main                 clean
```

### ws list - Various Scenarios

**Multiple Workspaces:**
```
NAME           BRANCH         REPOS  PATH
production     production         5  /Users/user/dev/workspaces/production
feature-auth   feature-auth       3  /Users/user/dev/workspaces/feature-auth
bugfix-123     bugfix-123         2  /Users/user/dev/workspaces/bugfix-123
```

**With Error:**
```
NAME        BRANCH  REPOS  PATH
prod        prod        5  /Users/user/dev/workspaces/prod
broken      ERROR   ?      /Users/user/dev/workspaces/broken
```

### ws repo list

```
IDENTITY                      SHORTNAME      URL
github.com/acme/api-gateway   api-gateway    git@github.com:acme/api-gateway.git
github.com/acme/auth          auth           git@github.com:acme/auth.git
gitlab.com/corp/payments      payments       git@gitlab.com:corp/payments.git
```

### ws group list

```
NAME       REPOS
backend        5
frontend       3
platform       8
```
