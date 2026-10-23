#!/usr/bin/env bash
# boundary-guard(1) — the architecture's one rule, checked mechanically.
#   ./scripts/boundary-guard.sh
#
# The rule (docs/ARCHITECTURE.md): dependencies point one way,
#     ui/tui → app → ports ← adapters,  and app depends on domain.
#
# | # | Check | Why |
# |---|-------|-----|
# | 1 | per-layer allowed `use crate::…` | an arrow flipping is a bug even if it compiles |
# | 2 | domain: no `unwrap`/`expect`/`panic!` outside `#[cfg(test)]` | external input is untrusted; errors are values |
# | 3 | `todo!` only in `src/domain` | scattered todos silently ship — `docs/EXERCISES.md` owns the list |
#
# Multi-line `use crate::{ … }` trees are flattened before matching, and line
# comments are stripped first, so module docs that *mention* a cross-layer
# import (like this file does) cannot fake a violation.

set -uo pipefail
cd "$(dirname "$0")/.."

failures=0

violation() {
    local why=$1 where=$2
    echo "boundary-guard: violation in $where" >&2
    echo "  $why" >&2
    failures=$((failures + 1))
}

# -- rule 1: the import table ------------------------------------------------
# Allowed top-level `crate::` names per layer. Strays in src root (lib.rs,
# main.rs, bin/) are wiring and exempt by design.
#
# The extraction is brace-aware because a use-tree nests:
# `use crate::{domain::{A, B}, ports};` — the top-level names are the ones
# that follow `crate::{` or a `,`, not the names inside a nested `::{…}`.
# That lexing is fiddly in pure shell, so it lives in a few lines of Python;
# the shell stays the front door.

# Print each top-level `crate::` name imported by the file, one per line.
# Reads the path as an argument: the heredoc carries the program, so the
# function takes a file rather than stdin — a piped-in file would never be
# seen (`python3 -` consumes the program from stdin and finds nothing left).
crate_imports() {
    python3 - "$1" <<'PY'
import sys, re

src = open(sys.argv[1], encoding="utf-8").read()

# Test modules import freely (fakes, wire types); the layer rule speaks for
# the shipped code. Tests sit at the file bottom, so cut the tail at
# `#[cfg(test)]` — the same convention rule 2 uses.
cut = src.find("#[cfg(test)]")
if cut >= 0:
    src = src[:cut]

src = re.sub(r"//[^\n]*", "", src)          # strip line comments; docs mention imports

for m in re.finditer(r"\buse\s+crate\s*::", src):
    i = m.end()
    # Two forms: `use crate::a::b::{X}` and `use crate::{a, b::{X}}`.
    # The names that must be checked are the crate-root items — the first
    # path component of the statement (path form), or every item at the
    # first brace level (brace form). Names *inside* a nested `::{…}` are
    # sub-paths and say nothing about the arrow between layers.
    base, depth = 0, 0
    pending, cur = True, ""
    # pending: the identifier being built starts a new root-level item.

    def flush(tok, is_pending, at_base):
        if tok and is_pending and at_base:
            print(tok)

    while i < len(src):
        c = src[i]
        if c == ";":
            flush(cur, pending, depth == base)
            break
        if c == ":":
            flush(cur, pending, depth == base)
            cur, pending = "", False
            i += 1                          # swallow the second colon
        elif c == "{":
            if pending and depth == 0 and base == 0:
                # The brace sits right after `crate::` — the statement is the
                # brace form, and this brace is its root level. Later braces
                # (after `::`) belong to sub-paths and stay nested.
                base, depth, pending = 1, 1, True
            else:
                depth += 1
                pending = False
            cur = ""
        elif c == "}":
            depth -= 1
            if depth < 0:
                break
            cur, pending = "", depth == base
        elif c == ",":
            flush(cur, pending, depth == base)
            cur, pending = "", depth == base
        elif c.isalnum() or c == "_":
            cur += c
        else:
            # whitespace or `*` (glob): token boundary, but *not* an item
            # boundary — do not touch pending.
            if cur:
                flush(cur, pending, depth == base)
            cur = ""
        i += 1
PY
}

check_layer() {
    local layer_name=$1 allowed=$2 file token
    shopt -s nullglob
    for file in "src/${layer_name}"/*.rs "src/${layer_name}/"*/"*.rs"; do
        [ -f "$file" ] || continue
        while IFS= read -r token; do
            case "$token" in
                ""|self|as|glob) continue ;;
            esac
            case "$allowed $layer_name" in
                *"$token"*) ;; # allowed
                *)
                    violation \
                        "src/${layer_name} imports crate::${token} — allowed here: ${layer_name} ${allowed}" \
                        "$file"
                    ;;
            esac
        done < <(crate_imports "$file")
    done
}

check_layer "domain" "error"
check_layer "ports" "domain error ports"
check_layer "adapters" "domain error ports adapters"
check_layer "app" "domain error ports app"
check_layer "tui" "domain error ports tui"
check_layer "ui" "domain error ports app ui"

# -- rule 2: domain never unwrap/expect/panics outside its tests -------------
# `#[cfg(test)]` onward is test code; domain tests sit at the file bottom and
# build constants by hand. `todo!(` is intentionally absent from the list.

for file in src/domain/*.rs src/domain/*/*.rs; do
    [ -f "$file" ] || continue
    for word in 'unwrap(' 'expect(' 'panic!(' 'unreachable!(' 'unimplemented!('; do
        lines=$(awk -v banned="$word" '
            /#\[cfg\(test\)\]/ { in_tests = 1 }
            !in_tests {
                line = $0
                sub(/\/\/.*$/, "", line)
                idx = index(line, banned)
                if (idx > 0) {
                    # Require the operator-ish shape: a macro bang (`.unwrap!`
                    # does not exist, so `panic!(` carries its own `!`), a
                    # method call dot, or a call after `= ` — prose like
                    # `expected by the test` must not match. `expect_err(`
                    # contains `expect_` + `err`, never `expect(`, given the
                    # scan word ends in `(`.
                    if (banned == "expect(") {
                        if (substr(line, idx - 1, 1) != ".") next
                    } else if (banned == "unwrap(") {
                        if (substr(line, idx - 1, 1) != ".") next
                    }
                    print NR ": " line
                }
            }
        ' "$file")
        if [ -n "$lines" ]; then
            violation \
                "domain must not call $word outside tests (${lines//$'\n'/; })" \
                "$file"
        fi
    done
done

# -- rule 3: todo! is the exercise marker, only in domain ---------------------

for todo in $(grep -rln 'todo!(' src --include='*.rs' 2>/dev/null | grep -v '^src/domain' || true); do
    violation \
        "todo! outside src/domain would silently ship — exercises are documented in docs/EXERCISES.md" \
        "$todo"
done

if [ "$failures" -gt 0 ]; then
    echo "boundary-guard: $failures violation(s); see docs/ARCHITECTURE.md" >&2
    exit 1
fi
echo "boundary-guard: layers held."
