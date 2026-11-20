#!/usr/bin/env bash
# The definition of done (plans/README.md §1.4) as one command.
#
#   tools/check.sh                      fmt, clippy, tests, deny, markers
#   tools/check.sh --plan <ID|path>     also the ownership check against
#                                         the plan's ```owns block
#   tools/check.sh --base <ref>         the ref the ownership diff starts
#                                         from (default: the integrate/*
#                                         branch this one forked from)
#   tools/check.sh --no-test            everything except cargo test
#
# Prints one line per step. Full output goes to <target dir>/check/<step>.log
# (CARGO_TARGET_DIR, else target/), and a failing step prints only its error
# lines. Runs that share a target dir take turns. Exit 0 when everything
# passes.
set -uo pipefail

root=$(git rev-parse --show-toplevel) || exit 2
cd "$root" || exit 2
# the logs live with the build they describe; two runs on one target dir
# would interleave them, so the second waits for the first
target=${CARGO_TARGET_DIR:-$root/target}
case "$target" in /*) ;; *) target="$root/$target" ;; esac
logs="$target/check"
mkdir -p "$logs"
exec 9>"$logs/.lock"
if ! flock -n 9; then
  printf 'wait  another check.sh is running on %s\n' "$target"
  flock 9
fi

plan="" base="" run_tests=1
while [ $# -gt 0 ]; do
  case "$1" in
    --plan) plan="$2"; shift 2 ;;
    --base) base="$2"; shift 2 ;;
    --no-test) run_tests=0; shift ;;
    -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
    *) echo "check.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done

failed=0
pass() { printf 'ok    %s\n' "$1"; }
fail() {
  printf 'FAIL  %s (full log: %s)\n' "$1" "$2"
  # the lines worth reading, not the whole log
  rg -n -m 30 '^(error|warning)(\[|:)|^ *--> |panicked at|^test .* FAILED|^failures:|^ {4}[a-z_:]+$' "$2" 2>/dev/null \
    | head -30 | sed 's/^/      /'
  failed=1
}
step() { # name, command...
  local name=$1; shift
  local log="$logs/$name.log"
  if "$@" >"$log" 2>&1; then pass "$name"; else fail "$name" "$log"; fi
}

step fmt cargo fmt --all -- --check
# clippy also reports every rustc warning, so this covers "zero warnings"
step clippy cargo clippy --workspace --all-targets --message-format short -- -D warnings
if [ "$run_tests" = 1 ]; then
  step test cargo test --workspace
  # one summary per test binary and doc-test run; a summary that output
  # from a test's own threads ran into is still found (no ^ anchor), and a
  # count that misses one says so instead of printing a wrong total
  if [ "$failed" = 0 ]; then
    runs=$(rg -c '^ *(Running|Doc-tests) ' "$logs/test.log")
    rg -o 'test result: ok\. [0-9]+ passed' "$logs/test.log" \
      | awk -v runs="${runs:-0}" '{s+=$4; n++} END {
          if (n == runs) printf "      %d tests passed\n", s
          else printf "      %d tests passed in %d of %d test runs (summaries missing: see the log)\n", s, n, runs
        }'
  fi
fi
if cargo deny --version >/dev/null 2>&1; then
  step deny cargo deny check
else
  printf 'skip  deny (cargo-deny is not installed: cargo install cargo-deny)\n'
fi

# markers: no todo!/unimplemented!/dbg! in source; unsafe only where allowed
markers="$logs/markers.log"
{
  rg -n 'todo!\(|unimplemented!\(|dbg!\(' -g '*.rs' blast/src blast_server/src blast_macros/src
  # tests/rt_alloc.rs needs an unsafe impl GlobalAlloc (its own crate)
  rg -n '\bunsafe\s*(\{|fn\b|impl\b|extern\b)' -g '*.rs' -g '!blast/tests/rt_alloc.rs' \
    blast blast_server blast_macros
} >"$markers" 2>&1
if [ -s "$markers" ]; then
  printf 'FAIL  markers (todo!/unimplemented!/dbg!/unsafe)\n'
  head -20 "$markers" | sed 's/^/      /'
  failed=1
else
  pass markers
fi

# ownership: every changed file must match a glob in the plan's ```owns block
if [ -n "$plan" ]; then
  case "$plan" in
    */*|*.md) plan_file=$plan ;;
    *) plan_file="/home/alexander-toth/rust_proj/blast/plans/detailed/$plan.md" ;;
  esac
  if [ ! -f "$plan_file" ]; then
    printf 'FAIL  ownership (no plan at %s)\n' "$plan_file"; failed=1
  else
    mapfile -t globs < <(awk '/^```owns/{f=1; next} /^```/{f=0} f && NF {print $1}' "$plan_file")
    if [ "${#globs[@]}" = 0 ]; then
      printf 'skip  ownership (%s has no ```owns block)\n' "$plan_file"
    else
      if [ -z "$base" ]; then
        # the integrate/* branch with the most recent merge-base is the one
        # this branch forked from
        best=0
        for ref in $(git for-each-ref --format='%(refname:short)' 'refs/heads/integrate/*'); do
          mb=$(git merge-base HEAD "$ref" 2>/dev/null) || continue
          t=$(git show -s --format=%ct "$mb")
          if [ "$t" -gt "$best" ]; then best=$t; base=$mb; fi
        done
      fi
      if [ -z "$base" ]; then
        printf 'FAIL  ownership (no base: pass --base <ref>)\n'; failed=1
      else
        outside=()
        while IFS= read -r f; do
          [ -z "$f" ] && continue
          ok=0
          for g in "${globs[@]}"; do
            # in a [[ == ]] pattern, * also matches /, so dir/** works
            # shellcheck disable=SC2053
            if [[ $f == $g ]]; then ok=1; break; fi
          done
          [ "$ok" = 0 ] && outside+=("$f")
        done < <( { git diff --name-only "$base"...HEAD; git diff --name-only HEAD; \
                    git ls-files --others --exclude-standard; } | sort -u)
        if [ "${#outside[@]}" = 0 ]; then
          pass "ownership (vs $(git rev-parse --short "$base"))"
        else
          printf 'FAIL  ownership: %d file(s) outside the plan'"'"'s owns block (vs %s)\n' \
            "${#outside[@]}" "$(git rev-parse --short "$base")"
          printf '      %s\n' "${outside[@]}" | head -20
          failed=1
        fi
      fi
    fi
  fi
fi

if [ "$failed" = 0 ]; then echo "check: all passed"; else echo "check: FAILED"; fi
exit "$failed"
