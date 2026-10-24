# Planner journal: PF-shell-repl

## Current job
Phase 1 planning done (2026-10-24). Waiting for architect review and user
decisions on the requests below. Next: revise plans on relayed decisions
(Job 2); before wave 2 refresh SHa/SHb, before wave 4 refresh REPLa/REPLb
against `integrate/wave-4` (R1's actual runtime layout).

## Plans
| File | Task | Wave | Status |
|---|---|---|---|
| detailed/SH-grammar.md | SH grammar proposal (user approval, REQ-004) | 2 | draft |
| detailed/SHa.md | SH part 1: lexer, parser, expressions | 2 | draft |
| detailed/SHb.md | SH part 2: interpreter, public API | 2 | draft |
| detailed/REPLa.md | REPL part 1: editing keys, history, Ctrl-R, long-line render | 4 | draft |
| detailed/REPLb.md | REPL part 2: shell integration, scripts, completion | 4 | draft |

SH split into SHa→SHb, REPL into REPLa→REPLb (sequential, same files).
REPLb adds a new file `runtime/script.rs` (needs ownership row + `mod script;`).

## Open requests
| ID | Type | Blocks | Topic |
|---|---|---|---|
| REQ-004 | question | SHa, SHb | approve shell grammar (D-1..D-7) |
| REQ-005 | architecture | SHb | §3.7 additive API; `source` via loader |
| REQ-006 | question | none | `all` expanded by command layer; who implements |
| REQ-007 | architecture | REPLb | `CmdProcessor::submit_batch`, all-or-nothing mirror (suggest E1) |
| REQ-008 | architecture | REPLb | script thread shares `Arc<Mutex<Submitter>>` vs channel |
| REQ-009 | question | REPLb | `completions` semantics (byte cursor, whole words) |
| REQ-010 | question | none | multi-line block editing: recommend out of scope |

Cross-checks still to do when other plans land: REQ-009 vs PH's REQ-017
(completion specs) and INS plan; REQ-007 vs PA's E1 plan (Batch); R1 plan's
key loop / redraw / print_line / where CmdProcessor is created (REPLa/b
"Consumes").

## Implementers
| Task | Agent | Branch | State |
|---|---|---|---|

## Review notes
