# Requests

Every question, change, approval, dispute or dispatch goes through a request
file here. File name: `REQ-NNN-<slug>.md`, numbered sequentially. To get the
next number, list this folder. If two roles collide on a number, the later one
renumbers.

## Types

| Type | Meaning | Route |
|---|---|---|
| `question` | needs an answer, changes nothing by itself | PM answers it from the docs, or queues it for the user |
| `plan-approval` | approve a new detailed plan | architect review, then the user |
| `plan-change` | change the scope, interfaces, ownership, behaviour or wave of an approved plan | architect review if a contract is touched, then the user |
| `dependency` | add a crate outside README §1.1 | architect, then the user |
| `architecture` | change README §3, or something of the same weight | architect designs and approves it, then the user (`ORG.md` §4.2) |
| `merge` | merge a wave into `dev` (or anything into `master`, or a push) | the user |
| `appeal` | the requester disputes an architect rejection | the user, shown both positions |
| `dispatch` | spawn an agent on behalf of a role that can't | PM spawns it, with no user approval needed |

## Status lifecycle

`open` → `answered` | `returned` | `with-architect` → (`design-rejected` | `awaiting-user`) → `approved` | `rejected` | `deferred`.
`withdrawn` is allowed at any time. `dispatch` requests go `open` → `done`.

## Template

```markdown
---
id: REQ-NNN
type: question | plan-approval | plan-change | dependency | architecture | merge | appeal | dispatch
from: <role> (<brief or task>)
tasks: [<task IDs affected>]
blocks: [<task IDs that can't proceed until this is decided>]
status: open
created: YYYY-MM-DD
---

## Request
One or two sentences.

## Context
Why it came up. Links to plan sections or code.

## Options
1. **<option>**: effect and cost. (Recommended)
2. **<option>**: effect and cost.

## Impact
Files, tasks, waves, interfaces affected, and the cost of the choice that
wasn't recommended.

## Architect review
(architect fills this in: verdict, plus a link to design/REQ-NNN.md)

## Decision
(PM fills this in: the user's choice, the date, and any conditions)
```

For `dispatch`, replace Options/Impact with: **agent type**, **isolation**,
**exact prompt**, and **who to report back to**.
