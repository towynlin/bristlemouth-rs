#!/usr/bin/env bash
# Report symbols libbm_core.a references but no object in it defines.
#
# Everything left should be libc. Anything else is an integrator hook the shim
# in csrc/ forgot to implement; it would otherwise surface much later as a
# confusing link error inside a test or a fuzz target.
#
# Run it from the workspace root. With --check it also fails when anything
# outside the allowlist below is left over, which is what CI runs; without it
# the script only prints, as it always has.
set -euo pipefail
export LC_ALL=C

check=false
if [[ ${1:-} == --check ]]; then
  check=true
fi

archive=$(ls -t target/debug/build/bm-wire-sys-*/out/libbm_core.a | head -1)
echo "archive: $archive"

defined=$(nm --defined-only "$archive" | awk '{print $NF}' | sort -u)
undefined=$(nm -u "$archive" | awk '/^ +U/ {print $NF}' | sort -u)

leftover=$(comm -23 <(echo "$undefined") <(echo "$defined"))
echo "$leftover"

$check || exit 0

# The libc surface bm_core and the shim are allowed to reach for. Deliberately
# not on it: rand, srand, time, clock_gettime and friends. csrc/ has to stay
# deterministic -- a fuzz input must replay byte-identically -- so a reference
# to one of those is a finding, not a yawn, and this list is where it shows up.
#
# Compiler and libc internals (__assert_fail, __stack_chk_fail, the soft-float
# builtins, _GLOBAL_OFFSET_TABLE_) are matched by pattern below instead:
# they vary with the host toolchain, and none of bm_core's integrator hooks
# are spelled that way.
libc=$(
  cat <<'EOF'
abort
abs
atoi
calloc
exit
fclose
fflush
fopen
fprintf
fputc
fputs
fread
free
fwrite
getenv
labs
longjmp
malloc
memcmp
memcpy
memmove
memset
printf
putchar
puts
qsort
realloc
setjmp
_setjmp
snprintf
sprintf
sscanf
stderr
stdout
strcat
strchr
strcmp
strcpy
strlen
strncat
strncmp
strncpy
strnlen
strrchr
strstr
strtol
strtoul
vfprintf
vprintf
vsnprintf
EOF
)

unexpected=$(
  echo "$leftover" |
    grep -v -E '^(_GLOBAL_OFFSET_TABLE_|__[A-Za-z0-9_]+)$' |
    comm -23 - <(echo "$libc" | sort -u)
)

if [[ -n $unexpected ]]; then
  echo
  echo "unresolved and not libc:"
  echo "$unexpected"
  echo
  echo "Each of these is either an integrator hook csrc/ still owes bm_core," \
    "or a libc function nothing here used before. Implement the former;" \
    "add the latter to the allowlist in this script."
  exit 1
fi
