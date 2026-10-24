# Planner journal: PJ-recording

## Current job
Job 1 (plan), phase 1: done 2026-10-24, plans handed to the PM.
Next: Job 2 (revise) when the PM relays architect fixes or decisions; refresh
against `integrate/wave-4` before dispatch (E1/R1 names: Garbage, emit,
discard, out_channels, CmdErr, parse-arm return type, R1 line printer).

## Plans
REC is split into three sequential parts, all wave 4, same owned files.
| Plan | Status | Depends on |
|---|---|---|
| `detailed/RECa.md` export writer (`export/**`) | draft | REQ-001, REQ-003 (step 4) |
| `detailed/RECb.md` live rec, render tap, commands | draft | REQ-001, RECa |
| `detailed/RECc.md` offline bounce | draft | REQ-002 = (a), RECb |

## Open requests
| ID | Type | Summary |
|---|---|---|
| REQ-001 | architecture | recording pipeline as README §3.10 (ring, writer thread, Command/Garbage/Event variants, notifier + finish_all hooks) |
| REQ-002 | architecture | bounce design: recommend (a) in-callback muted render; (b) rejected; (c) fallback |
| REQ-003 | question | >4 GB policy: recommend splitting into numbered files |

## Notes
- hound 3.5.1 checked: float32 + 24-bit OK, header patched on finalize/flush;
  `data_bytes_written: u32` overflows past 4 GiB (write.rs:436).
- Side effect while reading hound sources: running `rustc --version` inside
  the hound registry dir made rustup auto-install toolchain 1.40.0 (hound ships
  a toolchain pin). Harmless; can be removed with `rustup toolchain uninstall 1.40.0`.

## Implementers
| Task | Agent | Branch | State |
|---|---|---|---|

## Review notes
