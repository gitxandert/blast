# Requests

Every question, change, approval or dispute goes through a request
file here. File name: `REQ-NNN-<slug>.md`, numbered sequentially. To get the
next number, list this folder. The architect files them.

## Types

| Type | Meaning | Route |
|---|---|---|
| `question` | needs an answer, changes nothing by itself | architect answers it from the docs, or queues it for the user |
| `plan-approval` | approve a new detailed plan | the user |
| `plan-change` | change the scope, interfaces, ownership, behaviour or wave of an approved plan | the user |
| `dependency` | add a crate outside README §1.1 | the user (architect recommends) |
| `architecture` | change README §3, or something of the same weight | architect designs it, then the user (`ORG.md` §4.2) |
| `merge` | merge a wave into `dev` (or anything into `master`, or a push) | the user |

## Status lifecycle

`open` → `answered` | `awaiting-user` → `approved` | `rejected` | `deferred`.
Older requests may also show `returned`, `with-architect` or `design-rejected`
(from the retired planner/PM organisation).
`withdrawn` is allowed at any time.

## Template

```markdown
---
id: REQ-NNN
type: question | plan-approval | plan-change | dependency | architecture | merge
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

## Architect recommendation
(the architect's view and a link to design/REQ-NNN.md if there is one)

## Decision
(architect fills this in: the user's choice, the date, and any conditions)
```
