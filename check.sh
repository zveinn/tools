#!/usr/bin/env bash
#
# Build-check every project under x*/ for BOTH release architectures.
#
# Run this before tagging. .github/workflows/release.yml builds on native amd64
# and arm64 runners, and its publish job requires all of them to succeed, so a
# single arch-specific compile error blocks the release for every other project
# too. That is not hypothetical: libc::c_char is signed on x86_64 and unsigned
# on aarch64, so `[0i8; N]` passed to getpwuid_r built fine on amd64 and broke
# xls on arm64 (v0.0.1).
#
# Coverage note: Rust crates whose build scripts compile C (openssl-sys in xssh,
# aws-lc-sys in xgit) cannot be cross-checked without an aarch64 C toolchain.
# Those are reported as `skip`, not `FAIL`, so this script stays quiet enough to
# be worth running; CI's native arm64 runner is what covers them.
#
# Usage:
#   ./check.sh            # every x*/ project
#   ./check.sh xls xmux   # only the named ones

set -uo pipefail
cd "$(dirname "$0")"

ARM_TARGET=aarch64-unknown-linux-gnu

pass=0
fail=0
skip=0
failed_items=()

if [ -t 1 ]; then
    G=$'\033[32m'; R=$'\033[31m'; Y=$'\033[33m'; B=$'\033[1m'; N=$'\033[0m'
else
    G=""; R=""; Y=""; B=""; N=""
fi

ok()   { printf '  %sok%s    %s\n'   "$G" "$N" "$1"; pass=$((pass + 1)); }
bad()  { printf '  %sFAIL%s  %s\n'   "$R" "$N" "$1"; fail=$((fail + 1)); failed_items+=("$1"); }
warn() { printf '  %sskip%s  %s\n'   "$Y" "$N" "$1"; skip=$((skip + 1)); }

# Show the compiler's own error lines, indented. Build scripts for C-heavy
# crates emit dozens of `warning:` lines that bury the actual cause, so only
# error lines are kept; the full output is one command away anyway.
dump_errors() {
    local n=0
    while IFS= read -r line; do
        case "$line" in
            error*|*"error["*|*": error:"*)
                printf '        %s\n' "$line"
                n=$((n + 1))
                [ "$n" -ge 15 ] && { printf '        ...\n'; return 0; }
                ;;
        esac
    done <<<"$1"
    [ "$n" -eq 0 ] && printf '        (no error lines; re-run the command by hand)\n'
    return 0
}

# Projects to check: arguments if given, else every x*/ with a manifest.
projects=()
if [ "$#" -gt 0 ]; then
    projects=("$@")
else
    for d in x*/; do
        d="${d%/}"
        if [ -f "$d/Cargo.toml" ] || [ -f "$d/go.mod" ]; then
            projects+=("$d")
        fi
    done
fi

# Only pay for the arm64 std if a Rust project is actually in scope.
needs_rust=false
for d in "${projects[@]}"; do
    [ -f "$d/Cargo.toml" ] && needs_rust=true
done
if [ "$needs_rust" = true ] && command -v rustup >/dev/null; then
    if ! rustup target list --installed | grep -qx "$ARM_TARGET"; then
        printf '%sinstalling %s%s\n' "$B" "$ARM_TARGET" "$N"
        rustup target add "$ARM_TARGET" || exit 1
    fi
fi

for d in "${projects[@]}"; do
    printf '%s%s%s\n' "$B" "$d" "$N"

    if [ -f "$d/go.mod" ]; then
        for arch in amd64 arm64; do
            # Pure-Go static builds, so GOARCH alone cross-compiles. Discard the
            # binary; we only care that it compiles.
            if out=$(cd "$d" && CGO_ENABLED=0 GOOS=linux GOARCH="$arch" \
                go build -trimpath -o /dev/null . 2>&1); then
                ok "go build linux/$arch"
            else
                bad "$d go build linux/$arch"
                printf '%s\n' "$out" | sed 's/^/        /'
            fi
        done

    elif [ -f "$d/Cargo.toml" ]; then
        if out=$(cd "$d" && cargo check --locked --release 2>&1); then
            ok "cargo check amd64"
        else
            bad "$d cargo check amd64"
            dump_errors "$out"
        fi

        if out=$(cd "$d" && cargo check --locked --release --target "$ARM_TARGET" 2>&1); then
            ok "cargo check arm64"
        elif [[ "$out" == *"failed to run custom build command"* ]]; then
            # A -sys crate needs an aarch64 C toolchain we do not have. Name the
            # crate so it is obvious this is a tooling gap, not a code error.
            #
            # Matched with a bash glob rather than `grep -q`: under pipefail,
            # grep -q exits on the first match and SIGPIPEs the writer, so a
            # pipeline here would misclassify whenever cargo's output is large
            # enough that the writer has not finished (which is what aws-lc-sys
            # in xgit does). awk with `exit` for the same reason.
            crate=$(awk -F'`' '/failed to run custom build command for/ {print $2; exit}' <<<"$out")
            warn "cargo check arm64 (needs aarch64 C toolchain for ${crate:-a -sys crate}; CI covers it)"
        else
            bad "$d cargo check arm64"
            dump_errors "$out"
        fi

    else
        warn "no Cargo.toml or go.mod"
    fi
done

printf '\n%s%d ok, %d failed, %d skipped%s\n' "$B" "$pass" "$fail" "$skip" "$N"
if [ "$fail" -gt 0 ]; then
    printf '%sdo not tag:%s\n' "$R" "$N"
    for f in "${failed_items[@]}"; do
        printf '  - %s\n' "$f"
    done
    exit 1
fi
