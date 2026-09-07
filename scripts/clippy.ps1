# Dead-code guardrail for mev-scout (companion to docs/DEAD_CODE_CLEANUP_PLAN.md
# Phase 7). Run before pushing so dead code cannot accumulate again.
#
# Scope note: the plan originally suggested `-W dead_code -D warnings`, but the
# workspace still carries hundreds of pre-existing clippy *style* lints
# (too_many_arguments, precedence, useless_format, ...) that `-D warnings`
# would fail. This guardrail therefore errors only on dead_code - the class of
# finding the cleanup plan targets. Test binaries are excluded because they
# legitimately contain many per-binary unused helpers (each --test target
# compiles common/setup.rs separately).
#
# Usage: powershell -File scripts\clippy.ps1

$ErrorActionPreference = "Stop"

Write-Host "==> cargo clippy (dead-code errors on lib + bins)"
cargo clippy --workspace --lib --bins -- -W dead_code -D dead_code
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "==> clippy guardrail passed: no dead code in lib/bins"
