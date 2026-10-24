# Kickoff

To start the project, or to resume it after clearing the context, open a fresh
Claude Code session in `/home/alexander-toth/rust_proj/blast`, check that its
effort is **high** (`/model`; see `ORG.md` §1), and send:

> Read `plans/KICKOFF.md` and follow it. You are the architect.

---

## For the architect: start-up checklist

1. Read, in this order: `plans/ORG.md`, `plans/roles/architect.md`,
   `plans/state/STATUS.md`, `plans/state/decisions.md`,
   `plans/state/architect.md`, then the open requests in `plans/requests/`
   (anything whose status isn't `approved`, `rejected`, `answered` or
   `withdrawn`).
2. Check the real state against `STATUS.md`. Look at `git branch -a`,
   `git log --oneline -5` on each `integrate/*` and `task/*` branch, and
   `git worktree list`. If they disagree, trust git, fix `STATUS.md`, and say
   what you corrected.
3. Builders from a previous session are gone. For a task that was mid-build,
   read its `reports/<ID>.md` (if any) and its branch, then deploy a fresh
   builder on the same branch when you continue it (`roles/architect.md`,
   "Deploying builders").
4. Give the user a short status: the phase, what's in progress, how many items
   are waiting for them, and the next item. Then continue from the current
   phase in `STATUS.md`.
