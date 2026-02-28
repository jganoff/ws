default: check

# format code
fmt:
    cargo fmt

# format check + clippy
check:
    cargo fmt --check
    cargo clippy -- -D warnings
    cargo clippy --features codegen -- -D warnings

# generate SKILL.md from CLI introspection
skill: (build-bin "codegen")
    cargo run --release --features codegen -- setup skill generate > skills/wsp-manage/SKILL.md

# build a release binary, optionally with extra features
[private]
build-bin features="":
    {{ if features == "" { "cargo build --release" } else { "cargo build --release --features " + features } }}

# build release binary
build: check build-bin

# run all tests
test:
    cargo test -- --test-threads=1

# audit dependencies for known vulnerabilities
audit:
    cargo audit

# full CI pipeline (mirrors .github/workflows/ci.yml)
ci: check audit build test
    @echo "Checking SKILL.md freshness..."
    @cargo run --release --features codegen -- setup skill generate | diff -q - skills/wsp-manage/SKILL.md || (echo "SKILL.md is stale. Run 'just skill' to regenerate." && exit 1)

# auto-fix formatting and lint where possible
fix:
    cargo fmt
    cargo clippy --fix --allow-dirty -- -D warnings

# preview unreleased changelog
changelog:
    git cliff --unreleased

# dry-run a release (patch, minor, or major)
release level:
    cargo release {{level}}

# execute a release (patch, minor, or major)
release-execute level:
    cargo release {{level}} --execute

# install git pre-commit hook
install-hooks:
    #!/usr/bin/env sh
    hooks_dir="$(git rev-parse --git-common-dir)/hooks"
    mkdir -p "$hooks_dir"
    cat > "$hooks_dir/pre-commit" <<'HOOK'
    #!/usr/bin/env sh
    just check
    HOOK
    chmod +x "$hooks_dir/pre-commit"
    echo "pre-commit hook installed to $hooks_dir/pre-commit"
