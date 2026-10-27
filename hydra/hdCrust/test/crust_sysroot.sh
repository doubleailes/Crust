#!/usr/bin/env bash
# The release `crust` run under the pixi environment's sysroot loader and C
# library (glibc 2.39), the ones hdCrustTest and the plugin run under, so the
# CLI side of compare.sh and interaction.sh uses the same libm as the delegate
# and the two images can be bit-equal. Only for a host whose own glibc is
# older than the OpenUSD package needs (anderslanglands/aswf-pixi#57): there
# the plugin runs only through the sysroot's loader, and a CLI on the host's
# libm differs from it by an ulp on a few pixels. The pixi tasks
# `compare-sysroot` and `interaction-sysroot` (hydra/pixi.toml) set
# CRUST to this script; CI (ubuntu, glibc 2.39) runs the scripts as they are.
#
# Invoking the loader directly rather than patchelf-ing the binary: nothing is
# rewritten, so the same target/release/crust still runs on the host.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
SYSROOT="${CONDA_PREFIX:?run inside the hydra pixi environment}/x86_64-conda-linux-gnu/sysroot/lib64"
exec "$SYSROOT/ld-linux-x86-64.so.2" --library-path "$SYSROOT:$CONDA_PREFIX/lib" \
    "$ROOT/target/release/crust" "$@"
