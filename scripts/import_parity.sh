#!/usr/bin/env bash
# Import parity on one (large) scene: the current build against a baseline
# recorded from Crust's direct importer (the `moho-import` change, spec
# `import-parity`). This is how ALab and Moana are checked; the committed
# samples and the test fixtures are checked by `cargo test`.
#
# Usage:
#   scripts/import_parity.sh record <dir> <scene> [render flags...]
#   scripts/import_parity.sh check  <dir> <scene> [render flags...]
#
# `record` keeps in <dir>: the `crust check --json` report with the import's
# phases (timings, memory) removed, the 16 spp EXR (`--indirect-clamp 0`), and
# the import's peak memory and `Parse USD stage` time — the minimum over N runs.
# `check` does the same with the current build and compares: it exits 1 when
# the report or the image differs (`crust diff`), and prints both builds'
# memory and time with their ratios. Memory and time are reported, never gated:
# as UsdImaging did when it moved onto scene indices, parity is the gate.
#
# Give `check` the render flags `record` was given. Environment:
#   N=<runs>     renders per side for the timings (default 1; the image and
#                report come from the first)
#   BIN=<crust>  the binary (default target/release/crust, built if missing)
#
# Examples:
#   scripts/import_parity.sh record ~/baselines/alab samples/ALab/entry.usda -f 1004
#   scripts/import_parity.sh check  ~/baselines/alab samples/ALab/entry.usda -f 1004
#   scripts/import_parity.sh record ~/baselines/moana \
#       ~/Workspace/samples/island/usd/island.usda --camera /island/cam/shotCam

set -euo pipefail

MODE="${1:-}"
DIR="${2:-}"
SCENE="${3:-}"
EXTRA=("${@:4}")
N="${N:-1}"
BIN="${BIN:-target/release/crust}"

if { [ "$MODE" != record ] && [ "$MODE" != check ]; } || [ -z "$DIR" ] || [ -z "$SCENE" ]; then
    sed -n '2,27p' "$0"
    exit 2
fi
if [ ! -x "$BIN" ]; then
    cargo build --release -p crust-render
fi

mkdir -p "$DIR"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# The report without its import phases, and with the scene's directory written
# `$DIR`, so it compares across builds and checkouts.
strip_report() {
    python3 -I -c '
import json, sys, os, re
report = json.load(open(sys.argv[1]))
report.pop("import", None)
text = json.dumps(report, indent=2, sort_keys=True)
text = text.replace(os.path.dirname(os.path.abspath(sys.argv[2])), "$DIR")
# The listed parity exception (spec import-parity): openusd numbers the stage
# prototypes in the order they are first asked for, which the scene index
# population walk changes; the number is not compared, the rest of the path is.
text = re.sub(r"__Prototype_[0-9]+", "__Prototype_#", text)
print(text)
' "$1" "$SCENE"
}

# `<peak bytes> <parse seconds>` from a `crust-stats/1` report.
import_cost() {
    python3 -I -c '
import json, sys
s = json.load(open(sys.argv[1]))
parse = next(p for p in s["phases"] if p["name"] == "Parse USD stage")
peak = parse.get("peak_end") or s.get("peak_memory_bytes") or 0
print(peak, parse["time_s"])
' "$1"
}

# Renders N times into $1 (a prefix), keeping the first image and the
# minimum peak memory and parse time.
measure() {
    local out="$1" best_peak="" best_time=""
    "$BIN" check -i "$SCENE" "${EXTRA[@]}" --json "$WORK/check.json" -l error >/dev/null
    strip_report "$WORK/check.json" > "$out.check.json"
    for i in $(seq 1 "$N"); do
        "$BIN" render -i "$SCENE" "${EXTRA[@]}" -s 16 --indirect-clamp 0 \
            -o "$WORK/run$i.exr" --stats-json "$WORK/stats$i.json" -l error >/dev/null
        read -r peak time < <(import_cost "$WORK/stats$i.json")
        if [ -z "$best_peak" ] || [ "$peak" -lt "$best_peak" ]; then best_peak="$peak"; fi
        if [ -z "$best_time" ] || python3 -I -c "import sys; sys.exit(not float('$time') < float('$best_time'))"; then
            best_time="$time"
        fi
    done
    cp "$WORK/run1.exr" "$out.exr"
    echo "$best_peak $best_time" > "$out.cost"
}

if [ "$MODE" = record ]; then
    measure "$DIR/baseline"
    read -r peak time < "$DIR/baseline.cost"
    printf 'recorded %s: peak %.2f GiB, Parse USD stage %.2f s (min of %s)\n' \
        "$SCENE" "$(python3 -I -c "print($peak / 2**30)")" "$time" "$N"
    exit 0
fi

measure "$WORK/current"
status=0
if ! diff -u "$DIR/baseline.check.json" "$WORK/current.check.json" > "$WORK/report.diff"; then
    echo "REPORT DIFFERS from $DIR/baseline.check.json:"
    head -40 "$WORK/report.diff"
    status=1
fi
if ! "$BIN" diff "$DIR/baseline.exr" "$WORK/current.exr" >/dev/null 2>&1; then
    echo "IMAGE DIFFERS from $DIR/baseline.exr"
    status=1
fi
read -r bpeak btime < "$DIR/baseline.cost"
read -r cpeak ctime < "$WORK/current.cost"
python3 -I -c "
b_peak, b_time, c_peak, c_time = $bpeak, $btime, $cpeak, $ctime
gib = 2**30
print(f'peak memory:     baseline {b_peak / gib:8.2f} GiB   current {c_peak / gib:8.2f} GiB   ratio {c_peak / max(b_peak, 1):.3f}')
print(f'Parse USD stage: baseline {b_time:8.2f} s     current {c_time:8.2f} s     ratio {c_time / max(b_time, 1e-9):.3f}')
"
if [ "$status" = 0 ]; then
    echo "parity: report and image equal to the baseline"
fi
exit "$status"
