# Kickoff

To start the project, or to resume it after clearing the context, open a fresh
Claude Code session in `/home/alexander-toth/rust_proj/blast`, check that its
effort is **high** (`/model`; see `ORG.md` §7), and send:

> Read `plans/KICKOFF.md` and follow it. You are the project manager.

---

## For the PM: start-up checklist

1. Read, in this order: `plans/ORG.md`, `plans/roles/pm.md`,
   `plans/state/STATUS.md`, `plans/state/decisions.md`, then the open
   requests in `plans/requests/` (anything whose status isn't `approved`,
   `rejected`, `answered` or `withdrawn`).
2. Check the real state against `STATUS.md`. Look at `git branch -a`,
   `git log --oneline -5` on each `integrate/*` and `task/*` branch, and
   `git worktree list`. If they disagree, trust git, fix `STATUS.md`, and say
   what you corrected.
3. Agents from a previous session are gone. Respawn only what the current
   phase needs, giving each one its role file, its brief, and its journal under
   `plans/state/` (see `roles/pm.md`, "Spawning").
4. Give the user a short status: the phase, what's in progress, how many items
   are waiting for them, and the next item. Then continue from the current
   phase in `STATUS.md`.

If `STATUS.md` says **phase 0**, the project hasn't started. Begin as described
in `roles/pm.md`, under "Phase 0".
