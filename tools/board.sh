#!/usr/bin/env bash
# KICKOFF step 2 as a script: the real state of every task/* branch from
# git, next to what plans/state/STATUS.md says, with disagreements flagged.
#
#   tools/board.sh            all task branches
#   tools/board.sh --open     only branches not yet merged into dev
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 2

status=/home/alexander-toth/rust_proj/blast/plans/state/STATUS.md
open_only=0
[ "${1:-}" = "--open" ] && open_only=1

# task -> implementation column of STATUS.md's task table (5th column)
declare -A said
if [ -f "$status" ]; then
  while IFS=$'\t' read -r task impl; do said[$task]=$impl; done < <(
    awk -F'|' '/^\| [A-Za-z0-9]+ \|/ {
      t=$2; i=$6; gsub(/^ +| +$/, "", t); gsub(/^ +| +$/, "", i); print t "\t" i }' "$status")
fi

# worktree path per branch
declare -A wt
while read -r path branch; do wt[$branch]=$path; done < <(
  git worktree list --porcelain | awk '/^worktree /{p=$2} /^branch /{sub("refs/heads/","",$2); print p, $2}')

integrates=$(git for-each-ref --format='%(refname:short)' 'refs/heads/integrate/*')
flags=0
printf '%-10s %-8s %-22s %-30s %s\n' TASK SHA "GIT SAYS" "STATUS.md SAYS" WORKTREE
for ref in $(git for-each-ref --sort=-committerdate --format='%(refname:short)' 'refs/heads/task/*'); do
  id=${ref#task/}
  sha=$(git rev-parse --short "$ref")
  full=$(git rev-parse "$ref")
  if git for-each-ref --format='%(objectname)' refs/heads/dev 'refs/heads/integrate/*' | rg -qx "$full"; then
    # still at its base's tip: just created, nothing committed yet
    real="no commits yet"
  elif git merge-base --is-ancestor "$ref" dev; then
    real="in dev"
  else
    real="unmerged"
    for i in $integrates; do
      if git merge-base --is-ancestor "$ref" "$i"; then real="merged ${i#integrate/}"; break; fi
    done
  fi
  [ "$open_only" = 1 ] && [ "$real" = "in dev" ] && continue
  # split tasks (PKb1) may sit under their parent's row (PKb, PK)
  s=${said[$id]:-${said[${id%[0-9]}]:-${said[${id%[a-z0-9]}]:-}}}
  flag=""
  case "$real:$s" in
    unmerged:*merged*|unmerged:*in-dev*) flag=" <- STATUS says merged" ;;
    "in dev":*building*|merged*:*building*) flag=" <- STATUS says building" ;;
  esac
  [ -n "$flag" ] && flags=$((flags + 1))
  printf '%-10s %-8s %-22s %-30.30s %s%s\n' "$id" "$sha" "$real" "${s:--}" "${wt[$ref]:-}" "$flag"
done
echo
git worktree list | awk 'END {print NR " worktrees"}'
df -h "$(git rev-parse --show-toplevel)" | awk 'NR==2 {print "disk: " $5 " used, " $4 " free"}'
[ "$flags" -gt 0 ] && echo "$flags disagreement(s): trust git and fix STATUS.md (KICKOFF step 2)"
exit 0
