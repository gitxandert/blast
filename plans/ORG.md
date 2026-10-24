# Organisation: who does what, who approves what

```
          USER  (final authority; approves one item at a time)
            |
        ARCHITECT  (the active session: plans, structure, integration,
            |       runs the project, talks to the user)
            |
        BUILDERS   (background agents, one per task, in worktrees;
                    implement an approved plan themselves)
```

Two agent roles only (user decision, 2026-10-24). The **architect** is the
main Claude Code session, the one the user talks to. It writes every plan,
brings each one to the user for approval, and once a plan is approved it
**deploys a builder** to implement it. A **builder** implements exactly one
approved plan itself, in its own worktree, and reports back to the architect.
Nobody else is spawned: no planners, PM, implementers or design subagents.

Older files (briefs, detailed plans, requests, reports, `state/planners/`)
still mention planners, the PM and implementers. Read them as history: the
planner's and the PM's duties are now the architect's, and the implementer's
are the builder's.

## 1. Roles

| Role | Who runs it | Effort | Role file | Persistent state |
|---|---|---|---|---|
| **User** | you | n/a | n/a | `state/decisions.md` (your rulings, recorded by the architect) |
| **Architect** | the main session | **high** (`/model` → effort) | `roles/architect.md` | `README.md`, `briefs/`, `detailed/`, `design/`, `requests/`, `state/STATUS.md`, `state/decisions.md`, `state/architect.md` |
| **Builder** | one background agent per task, spawned by the architect | **medium** (`effort: "medium"` on every spawn) | `roles/builder.md` | its `task/<ID>` branch and `reports/<ID>.md` |

Effort levels are the user's choice. The architect doesn't change them without
the user's approval. Model: whatever the session default is; no override.

**Everything important lives on disk.** Agents don't survive a context clear,
so each role writes its state to the files above as it goes. Either role can
be restarted from its role file plus those files. Nothing is "remembered" in
conversation only.

## 2. Chain of communication

- The architect talks to the user directly (it's the active session) and
  brings decisions **one at a time**.
- Builders report **only to the architect**: in their final message, and in
  `reports/<ID>.md`. The architect answers builders with SendMessage.
- Anything that needs a user decision becomes a **request file**
  (`requests/README.md`), so nothing is lost when a context is cleared.

## 3. What needs whose approval

| Item | Who can approve it |
|---|---|
| A question already answered by approved docs (`README.md`, briefs, approved plans, `decisions.md`) | Architect answers it, citing the source, and logs it |
| An editorial plan fix: wording, a clearer step order, a test name, line references. It must not touch interfaces, ownership, scope or user-visible behaviour. | Architect, noted in the plan's changelog |
| A builder's fix-up iterations within an approved plan | Architect |
| Deploying a builder for a task whose plan is approved and whose wave has passed gate-in | Architect (the user's plan approval covers it) |
| Merging a `task/*` branch into `integrate/wave-N` (after the architect's review marks the report `ready`) | Architect |
| **A new detailed plan** (initial approval) | **User** |
| **A plan change**: scope, user-visible syntax or behaviour, interfaces, ownership, wave assignment | **User** |
| **A new dependency** outside README §1.1 | **User** (architect's recommendation in the request) |
| **Architecture change**: anything in README §3, or of the same weight (README §2, "Architecture rule") | **User** (architect's design in `design/REQ-NNN.md`) |
| Merging `integrate/wave-N` into `dev` (wave completion) | **User** |
| Merging into `master`, pushing, or deleting branches others might use | **User**, each time |

The architect doesn't review its own plans against itself: each plan states
its contract check (README §3, ownership, registry collisions, §1 ground
rules), and the user approves. The user approves items **one at a time**. The
architect never bundles several decisions into one question, and never treats
silence as approval.

## 4. The flows

### 4.1 Ordinary request (question, plan change, dependency)
1. A builder hits an issue and stops, or the architect finds one while
   planning. The architect answers it if its own authority covers it (§3).
   Otherwise it writes a request file.
2. The architect presents the request to the user: one item, a recommendation,
   and its impact. The user decides.
3. The architect records the decision in the request file and in
   `state/decisions.md`, applies it (plan, README, briefs), and tells any
   waiting builder.

### 4.2 Architecture change
1. The architect writes a design in `design/REQ-NNN.md`: the problem, 2–3
   options with their costs, the recommendation, and its exact effect on
   README §3, the briefs, ownership and approved plans.
2. The architect presents it to the user. If the user approves it, the
   architect updates README §3 (and §4), the briefs, the ownership tables, and
   the affected plans.

### 4.3 Planning
1. The architect writes `detailed/<task-id>.md` for each task (template in
   `roles/architect.md`), plus a request for each open question or
   architecture item, in wave order.
2. For each plan, the architect files a `plan-approval` request and presents it
   to the user. Later waves' plans keep being written and approved while
   earlier waves are being built; each must be approved before its own wave
   starts.

### 4.4 Running a wave (phases 2–5, waves 1–4)
1. **Gate in.** Every plan in the wave is user-approved, and the previous wave
   has been merged into `dev`.
2. **Scaffold.** The architect creates `integrate/wave-N` from `dev` and adds
   the wave's registry entries (README §2). The crate must still build and
   test green.
3. **Refresh.** The architect re-reads the code on `integrate/wave-N` and
   refreshes each plan (editorial fixes only; anything bigger is a request).
4. **Build.** The architect deploys one builder per task. Tasks with disjoint
   ownership may run in parallel; parts of a split task (`<ID>a`, `<ID>b`, …)
   that own the same files run in sequence, each builder branching from the
   previous part's tip. At most one builder per task at a time.
5. **Review.** When a builder reports, the architect checks its branch against
   the review checklist (`roles/architect.md`) and sends it back for fixes as
   needed (SendMessage, or a fresh builder on the same branch with the review
   notes). When it passes, the architect marks `reports/<ID>.md` `ready`.
6. **Integrate.** The architect merges ready branches into `integrate/wave-N`
   in the order of the README tables, runs the full build, test and clippy,
   and fixes merge-level problems in registry regions (or files a request if a
   fix would change a plan).
7. **Gate out.** The architect presents a `merge` request for the wave (tasks,
   reports, test results, deviations). With the user's approval, it merges the
   wave into `dev`.

## 5. Git authority

This document is the user's standing authorisation for exactly these actions:
- Builders create and commit to their own `task/<ID>` branch, in their
  worktree.
- The architect creates `integrate/wave-N`, commits scaffolding to it, and
  merges ready `task/*` branches into it.
- The architect commits changes **under `plans/` only** to `dev` in the main
  checkout, as snapshots: at every phase or wave gate, and after recording each
  user decision. The message is `plans: <what changed>`. No source files go in
  these commits.

Anything else that writes to git needs the user's approval each time: merging
into `dev` or `master`, pushing to any remote, rewriting history on shared
branches, or deleting branches. Commit messages follow README §1.3 and end
with the attribution trailer the session's system prompt specifies.

## 6. Files

**Location.** The live copy of `plans/` is in the main checkout at
`/home/alexander-toth/rust_proj/blast/plans/`. It's committed on `dev` (the
architect snapshots it, §5). Worktrees contain a copy that's frozen at their
base commit, so that copy is **stale**: ignore it, and never edit `plans/`
inside a worktree (it would show up in task diffs). Every role reads and
writes plans **by the absolute path** above. Code changes happen only in
worktrees. The main checkout stays on `dev`; the only things that change it
are the architect's `plans/` snapshots and its merge of a wave into `dev` with
the user's approval.

```
plans/
  ORG.md            this file
  KICKOFF.md        how to start or resume the project
  README.md         architecture, contracts, waves, ownership
  roles/            architect.md, builder.md
  briefs/           task scopes per area (architect's planning input)
  detailed/         task plans (architect-written; user-approved)
  design/           architecture-change designs
  reports/          per-task reports (builder writes, architect reviews)
  requests/         the decision queue (README.md has the format)
  state/
    STATUS.md       project board
    decisions.md    append-only log of user decisions
    architect.md    architect's journal and review notes
    planners/       frozen journals from the retired planners (history)
```
