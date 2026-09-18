#!/usr/bin/env bash
# Report symbols libbm_core.a references but no object in it defines.
#
# Everything left should be libc. Anything else is an integrator hook the shim
# in csrc/ forgot to implement; it would otherwise surface much later as a
# confusing link error inside a test or a fuzz target.
set -euo pipefail
export LC_ALL=C

archive=$(ls -t target/debug/build/bm-wire-sys-*/out/libbm_core.a | head -1)
echo "archive: $archive"

defined=$(nm --defined-only "$archive" | awk '{print $NF}' | sort -u)
undefined=$(nm -u "$archive" | awk '/^ +U/ {print $NF}' | sort -u)

comm -23 <(echo "$undefined") <(echo "$defined")
