#!/usr/bin/env bash
# Lists git worktrees that can go: their HEAD is already in dev or in an
# integrate/* branch, and they have no uncommitted changes. Removing a
# worktree keeps its branch (deleting branches needs the user, ORG.md §5).
#
#   tools/prune-worktrees.sh           dry run: what would be removed
#   tools/prune-worktrees.sh --apply   remove them (git worktree remove)
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 2
apply=0
[ "${1:-}" = "--apply" ] && apply=1

main=$(git worktree list --porcelain | awk '/^worktree /{print $2; exit}')
targets="dev $(git for-each-ref --format='%(refname:short)' 'refs/heads/integrate/*')"
n=0
while read -r path head label; do
  [ "$path" = "$main" ] && continue
  [ -d "$path" ] || continue
  merged=""
  for t in $targets; do
    # a branch's own worktree stays while that branch is still unmerged
    [ "$label" = "$t" ] && continue
    if git merge-base --is-ancestor "$head" "$t"; then merged=$t; break; fi
  done
  if [ -z "$merged" ]; then continue; fi
  # a branch still at a base tip is a task that has just started
  others=$(for t in $targets; do [ "$t" != "$label" ] && printf 'refs/heads/%s ' "$t"; done)
  # shellcheck disable=SC2086
  if git for-each-ref --format='%(objectname)' $others | rg -qx "$head"; then
    printf 'keep    %-60s %-14s no commits yet\n' "$path" "$label"; continue
  fi
  # a checkout or commit in the last 3 hours may have a builder in it
  # (logs/HEAD, not the index: git status refreshes the index)
  gd=$(git -C "$path" rev-parse --absolute-git-dir 2>/dev/null)
  if [ -n "$(fd -d 1 --changed-within 3h '^HEAD$' "$gd/logs" 2>/dev/null)" ]; then
    printf 'keep    %-60s %-14s active in the last 3 hours\n' "$path" "$label"; continue
  fi
  dirty=$(git -C "$path" status --porcelain --untracked-files=normal 2>/dev/null | head -1)
  size=$(du -sh "$path" 2>/dev/null | cut -f1)
  if [ -n "$dirty" ]; then
    printf 'keep    %-60s %-14s uncommitted changes\n' "$path" "$label"
    continue
  fi
  n=$((n + 1))
  if [ "$apply" = 1 ]; then
    # --force: build output in target/ is untracked but ignored, so git
    # would refuse otherwise; the status check above already found it clean
    git worktree remove --force "$path" && printf 'removed %-60s %-14s (in %s, %s)\n' "$path" "$label" "$merged" "$size"
  else
    printf 'remove  %-60s %-14s (in %s, %s)\n' "$path" "$label" "$merged" "$size"
  fi
done < <(git worktree list --porcelain | awk '
  /^worktree /{p=$2; b="(detached)"} /^HEAD /{h=$2}
  /^branch /{b=$2; sub("refs/heads/","",b)}
  /^$/{print p, h, b} END{if (p) print p, h, b}' | sort -u)

if [ "$apply" = 0 ]; then
  echo "$n worktree(s) can go. Run with --apply to remove them (branches stay)."
fi
git worktree prune
