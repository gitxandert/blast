# Organisation: who does what, who approves what

> **Reduced organisation (user decision, 2026-10-24; supersedes the rest of
> this file where they conflict).** To cut usage, only **two agents** run: the
> **PM** (main session) and the **architect** (background agent, spawned or
> resumed per job, never more than one). No planners, implementers or design
> subagents are spawned. Their duties move as follows:
> - **Plans:** the architect writes every remaining plan and request, and
>   applies fixes to existing plans (it takes over `detailed/`, `reports/` is
>   the PM's, planner journals are frozen history). The architect doesn't
>   review its own plans against itself: it states the contract check in the
>   plan, and the user approves as before.
> - **Implementation:** the PM implements each approved plan itself, **one task
>   at a time**, following `roles/implementer.md` (work, stop-and-report and
>   finish rules), in a worktree on `task/<ID>` branched from `integrate/wave-N`.
>   Where the implementer role says "report to your planner", the PM writes
>   `reports/<ID>.md` instead, using the planner's report template.
> - **Review and merge:** the architect reviews each finished task branch with
>   the planner's review checklist (`roles/planner.md`), then merges it into
>   `integrate/wave-N` as before. Fix-ups go back to the PM.
> - **Git:** in addition to §5, the PM may create `task/<ID>` branches and
>   worktrees and commit to them.
> - Approvals (§3) are unchanged: the user still approves plans, plan changes,
>   architecture, dependencies and wave merges, one at a time.
> - Dispatch requests no longer exist. Planner-authority items (§3, editorial
>   plan fixes) go to the architect.

```
                    USER  (final authority; approves one item at a time)
                   /    \
          ARCHITECT  <-->  PROJECT MANAGER (PM)
     (left hand:            (right hand: runs the project,
      structure, design,     triages, and is the active session)
      integration)                |
         |                   PLANNERS  (one per brief in plans/briefs/)
   design subagents               |
   (temporary)               IMPLEMENTERS  (one per task, in worktrees)
```

The **architect and the PM are peers**, the user's left and right hands. Both
answer only to the user, and neither reports to the other. The PM is the
active session, so it also acts as the **relay** for the architect: between
planners and the architect, and between the user and the architect (§2).

## 1. Roles

| Role | Who runs it | Role file | Persistent state |
|---|---|---|---|
| **User** | you | n/a | `state/decisions.md` (your rulings) |
| **PM** | the main Claude Code session (the one you talk to) | `roles/pm.md` | `state/STATUS.md`, `requests/` |
| **Architect** | a background agent the PM spawns on the user's behalf (it answers to the user, not the PM) | `roles/architect.md` | `README.md` (architecture), `design/`, `state/architect.md` |
| **Planner** | one background agent per brief, spawned by the PM | `roles/planner.md` | `detailed/`, `reports/`, `state/planners/<brief>.md` |
| **Implementer** | one agent per task, in its own git worktree | `roles/implementer.md` | its `task/<id>` branch, plus the report it hands to its planner |
| **Design subagent** | temporary agent for the architect | brief written by the architect | `design/` |

**Everything important lives on disk.** Agents don't survive a context clear,
so each role writes its state to the files above as it goes. Any role can be
respawned from its role file plus those files. Nothing is "remembered" in
conversation only.

**Spawning.** The PM spawns the architect and the planners. The architect
spawns design subagents, and planners spawn their implementers. If an agent's
toolset doesn't include the Agent tool (nested spawning isn't guaranteed), it
writes a **dispatch request** instead (`requests/`, type `dispatch`). The PM
spawns the agent exactly as described there, with no user approval needed, and
the requesting role keeps oversight of it as if it had spawned the agent itself.

## 2. Chain of communication

- Implementers report **only to their planner**.
- Planners report **only to the PM**.
- The architect answers **only to the user**. Because the architect is a
  background agent and the PM is the active session, the PM carries messages
  in both directions:
  - **Planners ↔ architect.** The PM passes planners' plans, questions and
    disputes to the architect, and passes the architect's reviews, fixes and
    rejections back.
  - **User ↔ architect.** The PM presents the architect's designs and
    recommendations to the user, and passes the user's decisions and questions
    back. When the user wants to talk to the architect directly, the PM relays
    the user's words verbatim and returns the architect's full reply.
  - The PM relays **faithfully**. It may summarise for the user, but it always
    links the architect's own text (the design file, or the request's
    "Architect review" section). It never edits, softens or overrides an
    architect verdict. Where the PM disagrees with the architect, it says so
    *separately*, as its own view, and the user decides.
- The architect and the PM coordinate as peers (for example, on scheduling
  architect work around waves). If they disagree on something within their own
  domains, the PM puts it to the user and shows both positions.
- Anything that needs a decision becomes a **request file** (`requests/README.md`).
  Requests are the only channel for questions, changes, approvals and disputes,
  so nothing gets lost when a context is cleared.
- Anything that needs a decision becomes a **request file** (`requests/README.md`).
  Requests are the only channel for questions, changes, approvals and disputes,
  so nothing gets lost when a context is cleared.

## 3. What needs whose approval

| Item | Who can approve it |
|---|---|
| A question already answered by approved docs (`README.md`, briefs, approved plans, `decisions.md`) | PM answers it, citing the source, and logs it. No user approval needed. |
| An editorial plan fix: wording, a clearer step order, a test name. It must not touch interfaces, ownership, scope or user-visible behaviour. | Planner, noted in the plan's changelog |
| An implementer's fix-up iterations within an approved plan | Planner |
| A dispatch request (spawning an agent another role asked for) | PM |
| Merging a `task/*` branch into `integrate/wave-N` (after the planner's report says ready) | Architect |
| **A new detailed plan** (initial approval) | Architect reviews it, then the **user** approves it |
| **A plan change**: scope, user-visible syntax or behaviour, interfaces, ownership, wave assignment | **User** (architect review first if it touches a contract) |
| **A new dependency** outside README §1.1 | Architect reviews it, then the **user** approves it |
| **Architecture change**: anything in README §3, or of the same weight (README §2, "Architecture rule") | The **architect** designs and approves it first (with design subagents if needed), then the **user** approves it |
| Merging `integrate/wave-N` into `dev` (wave completion) | **User** |
| Merging into `master`, pushing, or deleting branches others might use | **User**, each time |

The user approves items **one at a time**. The PM never bundles several
decisions into one question, and never treats silence as approval.

## 4. The flows

### 4.1 Ordinary request (question, plan change, dependency)
1. An implementer hits an issue and stops. The planner writes a request file,
   or answers it if the planner's own authority covers it (§3).
2. The PM triages. It answers from the docs, merges duplicates, rejects
   anything out of scope back to the planner with a reason, or puts it in the
   user queue. Requests that touch a contract get an architect review first.
3. The PM presents the request to the user: one item, a recommendation, and its
   impact. The user decides.
4. The PM records the decision in the request file and in `state/decisions.md`,
   then tells the requester.

### 4.2 Architecture change
1. The PM sends the request to the **architect** (status `with-architect`).
2. The architect writes a design in `design/REQ-NNN.md`. It may spawn design
   subagents to draft alternatives or rework the affected plans, and it reviews
   their output.
3. The architect then either **rejects** the request (with reasons; the PM
   tells the requester, who may revise it or appeal to the user through the PM)
   or **approves the design** (status `awaiting-user`).
4. The PM presents the architect-approved design to the user. If the user
   approves it, the architect updates `README.md` §3 and the affected briefs,
   and the affected planners revise their plans.

### 4.3 Planning (phase 1)
1. The PM spawns the 6 planners in parallel, one per brief. Planners only read
   code. They write only their own files under `plans/`.
2. Each planner writes `detailed/<task-id>.md` for every task in its brief,
   plus a request file for each open question or architecture item.
3. The architect reviews every plan for contract conformance and cross-plan
   consistency (`state/architect.md` holds the review notes). It sends fixes
   back through the PM.
4. The PM presents each plan to the user for approval (one `plan-approval`
   request per plan, in wave order).

### 4.4 Running a wave (phases 2–5, waves 1–4)
1. **Gate in.** Every plan in the wave is user-approved, and the previous wave
   has been merged into `dev`.
2. **Scaffold.** The architect creates `integrate/wave-N` from `dev` and adds
   the wave's registry entries (README §2). For wave 1 it also does scaffold
   step 0. The crate must still build and test green.
3. **Implement.** Each planner refreshes its plans against the actual code
   (editorial fixes only; anything bigger is a request), then dispatches one
   implementer per task in the wave.
4. **Review.** The planner checks each implementer's branch against the review
   checklist (`roles/planner.md`), sends it back for fixes as needed, and
   writes `reports/<task-id>.md` marked `ready`.
5. **Integrate.** The architect merges ready branches into `integrate/wave-N`
   in the order of the README tables, runs the full build, test and clippy,
   and fixes merge-level problems (or files a request if a fix would change a
   plan).
6. **Gate out.** The PM presents a wave-completion summary (tasks, reports,
   test results, deviations). With the user's approval, the architect merges
   the wave into `dev`.

## 5. Git authority

This document is the user's standing authorisation for exactly these actions:
- Implementers commit to their own `task/<id>` branch.
- The architect creates `integrate/wave-N` and commits scaffolding and merges to it.
- The PM commits changes **under `plans/` only** to `dev` in the main checkout, as
  snapshots: at every phase or wave gate, and after recording each user
  decision. The message is `plans: <what changed>`. No source files go in
  these commits.

Anything else that writes to git needs the user's approval each time: merging
into `dev` or `master`, pushing to any remote, rewriting history on shared
branches, or deleting branches. Commit messages follow README §1.3 and end
with the attribution trailer the session's system prompt specifies.

## 6. Files

**Location.** The live copy of `plans/` is in the main checkout at
`/home/alexander-toth/rust_proj/blast/plans/`. It's committed on `dev` (the PM
snapshots it, §5). Worktrees contain a copy that's frozen at their base commit,
so that copy is **stale**: ignore it, and never edit `plans/` inside a worktree
(it would show up in task diffs). Every role reads and writes plans **by the
absolute path** above. Code changes happen only in worktrees. The main checkout
stays on `dev`; the only things that change it are the PM's `plans/` snapshots
and the architect merging a wave into `dev` with the user's approval.


```
plans/
  ORG.md            this file
  KICKOFF.md        how to start or resume the project
  README.md         architecture, contracts, waves, ownership (architect-owned)
  roles/            pm.md, architect.md, planner.md, implementer.md
  briefs/           one per planner (architect-owned)
  detailed/         task plans (planner-owned; user-approved)
  design/           architecture-change designs (architect-owned)
  reports/          per-task review reports (planner-owned)
  requests/         the request queue (README.md has the format)
  state/
    STATUS.md       project board (PM-owned)
    decisions.md    append-only log of user decisions (PM-owned)
    architect.md    architect's journal and review notes
    planners/<brief>.md   each planner's journal
```

## 7. Effort levels (set by the user)

| Role | Effort |
|---|---|
| PM | **high**. It's the main session: start it with high effort (`/model` → effort). |
| Architect | **high** (`effort: "high"` on every spawn) |
| Planners | **medium** (`effort: "medium"`) |
| Implementers | **medium** (`effort: "medium"`) |
| Design subagents | not set by the user; the architect leaves `effort` unset unless the user specifies it |

Whoever spawns an agent sets its `effort` (on a dispatch request, the request
states it). Model: whatever the session default is; no override.

