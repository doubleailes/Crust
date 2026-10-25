#!/usr/bin/env bash
# The import-parity image run (the `moho-import` change, spec `import-parity`):
# every import the test suite performs is imported a second time and rendered
# at 16 spp with the indirect clamp off, and a hash of its pixels compared with
# the baseline recorded from the direct importer (`crust_core::parity`).
#
# Usage:
#   scripts/parity_images.sh          # compare
#   scripts/parity_images.sh record   # record the image baselines
#
# Optimised but without LTO (which moves no bits, see the workspace
# Cargo.toml) to keep the build short. The second import and its render are
# visible to the few tests that count asset requests or log lines, time a
# render, or render a material that panics on purpose; those fail in this mode
# and are listed in scripts/parity_images.expected. The run passes when no
# import differs from its baseline (no `parity:` panic) and every other failure
# is a listed one.

set -uo pipefail
cd "$(dirname "$0")/.."

export CRUST_PARITY_IMAGES=1
if [ "${1:-}" = record ]; then
    export CRUST_RECORD_BASELINES=1
fi
LOG="$(mktemp)"
trap 'rm -f "$LOG"' EXIT
CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
    cargo test --release --workspace --no-fail-fast >"$LOG" 2>&1

status=0
if grep -q "^parity: \|panicked.*parity.rs" "$LOG"; then
    echo "IMPORTS DIFFER FROM THEIR BASELINES:"
    grep -A3 "parity.rs" "$LOG" | grep "^parity:\|baseline:\|import:" | head -60
    status=1
fi
failed="$(grep -E '^test .* FAILED$' "$LOG" | sed 's/^test //; s/ \.\.\. FAILED$//' | sort -u)"
unexpected="$(comm -23 <(printf '%s\n' "$failed" | sed '/^$/d') <(sort -u scripts/parity_images.expected))"
if [ -n "$unexpected" ]; then
    echo "TESTS FAILING THAT ARE NOT EXPECTED TO IN THIS MODE:"
    printf '%s\n' "$unexpected"
    status=1
fi
if [ "$status" = 0 ]; then
    echo "parity images: every import renders as its baseline"
fi
exit "$status"
