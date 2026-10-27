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
# and are listed in scripts/parity_images.expected (`#` starts a comment
# there, giving each entry's reason).
#
# The run passes only when:
# - cargo built every test target and each one reported a result (a build
#   error, or a test binary that dies before its summary, fails the run);
# - no import differs from its baseline: every `parity:` diagnostic in the log
#   fails the run, whichever test or `crust` process raised it, unless it is
#   an image difference of a stage listed as `stage:<file>` in the expected
#   file (a stage whose render is known to vary from run to run, with its
#   reason; its digest is still compared);
# - every failed test is listed by name (it fails here for a reason that is
#   not an import difference, which the rule above still checks), or failed
#   only on excused image differences.

set -uo pipefail
cd "$(dirname "$0")/.."

export CRUST_PARITY_IMAGES=1
if [ "${1:-}" = record ]; then
    export CRUST_RECORD_BASELINES=1
fi
LOG="${PARITY_IMAGES_LOG:-}"
if [ -z "$LOG" ]; then
    LOG="$(mktemp)"
    trap 'rm -f "$LOG"' EXIT
    CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
        cargo test --release --workspace --no-fail-fast >"$LOG" 2>&1
    cargo_status=$?
else
    # PARITY_IMAGES_LOG names the log of a run already made (and
    # PARITY_IMAGES_STATUS its exit status): the checks below, without the run.
    cargo_status="${PARITY_IMAGES_STATUS:-0}"
fi

status=0
entries="$(sed 's/#.*//; s/[[:space:]]*$//; /^$/d' scripts/parity_images.expected | sort -u)"
tests_listed="$(printf '%s\n' "$entries" | grep -v '^stage:' || true)"
stages_listed="$(printf '%s\n' "$entries" | sed -n 's/^stage://p')"

# The build, and a result from every test target.
if grep -qE '^error(\[E[0-9]+\])?: ' "$LOG" \
    && grep -vE '^error: ([0-9]+ targets? failed|test failed)' "$LOG" | grep -qE '^error(\[E[0-9]+\])?: '; then
    echo "THE TESTS DID NOT BUILD OR RUN:"
    grep -E '^error' "$LOG" | grep -vE '^error: ([0-9]+ targets? failed|test failed)' | head -20
    status=1
fi
silent="$(awk '
    /^ *(Running|Doc-tests) / { if (pending) print target; target = $0; pending = 1; next }
    /^test result: / { pending = 0 }
    END { if (pending) print target }' "$LOG")"
if [ -n "$silent" ]; then
    echo "TEST TARGETS THAT REPORTED NO RESULT:"
    printf '%s\n' "$silent"
    status=1
fi

# A `parity:` diagnostic is excused only when it is an image difference
# (`parity: importing <stage> differs from its baseline <...>.image`) of a
# listed stage: a digest difference, a missing baseline or a failed
# re-import never is.
excused() {
    local stage
    stage="$(printf '%s\n' "$1" | sed -nE 's/.*parity: importing ([^ ]+) differs from its baseline [^ ]+\.image,.*/\1/p' | sed 's#.*/##')"
    [ -n "$stage" ] && printf '%s\n' "$stages_listed" | grep -qxF "$stage"
}

# Every `parity:` diagnostic, whichever test or process raised it.
unexplained="$(grep -E 'parity: ' "$LOG" | while IFS= read -r line; do
    excused "$line" || printf '%s\n' "$line"
done)"
if [ -n "$unexplained" ]; then
    echo "IMPORTS DIFFER FROM THEIR BASELINES:"
    printf '%s\n' "$unexplained" | head -60
    status=1
fi

# Each failed test, judged by its `---- <name> stdout ----` section: listed
# by name, or failing only on excused image differences.
failed="$(grep -E '^test .* FAILED$' "$LOG" | sed 's/^test //; s/ \.\.\. FAILED$//' | sort -u)"
unexpected="$(printf '%s\n' "$failed" | sed '/^$/d' | while IFS= read -r name; do
    printf '%s\n' "$tests_listed" | grep -qxF "$name" && continue
    diagnostics="$(awk -v n="---- $name stdout ----" '
        $0 == n { on = 1; next }
        on && (/^---- .* stdout ----$/ || /^failures:$/) { on = 0 }
        on { print }' "$LOG" | grep -E 'parity: ')"
    only_excused=1
    [ -z "$diagnostics" ] && only_excused=0
    while IFS= read -r line; do
        [ -z "$line" ] || excused "$line" || only_excused=0
    done <<<"$diagnostics"
    [ "$only_excused" = 1 ] || printf '%s\n' "$name"
done)"
if [ -n "$unexpected" ]; then
    echo "TESTS FAILING THAT ARE NOT EXPECTED TO IN THIS MODE:"
    printf '%s\n' "$unexpected"
    status=1
fi
if [ "$status" = 0 ] && [ "$cargo_status" != 0 ] && [ -z "$failed" ]; then
    echo "CARGO FAILED (exit $cargo_status) WITHOUT A FAILED TEST:"
    tail -20 "$LOG"
    status=1
fi
if [ "$status" = 0 ]; then
    echo "parity images: every import renders as its baseline"
fi
exit "$status"
