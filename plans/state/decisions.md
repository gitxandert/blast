# User decisions (append-only, PM-owned)

Each entry records the date, the decision, and its source (a request ID, or
"pre-kickoff" for decisions made while the plans were being written).

## Pre-kickoff (2026-10-24)

1. Use f32 for internal samples. (README §4.1)
2. Shell syntax: start from `notes.txt`, plus `let`/`for`/`if`/`fn`/`source`.
   The concrete grammar needs user approval. (§4.2)
3. macOS/Windows: deferred. No platform work planned or tested. (§4.3)
4. AAC: LC only. (§4.4)
5. Raw PCM: parameters from the filename convention; `import` flags override
   them. (§4.5)
6. Recording: 32-bit float WAV, tapped before the clamp, with optional
   16/24-bit dithered output. (§4.6)
7. Subfolder track naming: the LOAD planner proposes a scheme for user
   approval. (§4.7)
8. Dependencies: the approved set in README §1.1. Any other crate needs
   approval. (§4.8)
9. Decoders: symphonia, plus hand-written raw and AU. 8SVX, RF64, f16 WAV and
   AIFC ima4 are dropped. (§4.9)
10. Audio output: cpal. (§4.10)
11. Terminal: crossterm, keeping our own line editor. (§4.11)
12. Engine: slot arenas with stable IDs, no `Rc`/`RefCell`. (§4.12)
13. Coding conventions may change (README §1.3). Rewriting code is welcome.
    Fundamental architectural changes need user approval.
14. Organisation: user → architect + PM → planners → implementers, as defined
    in `ORG.md`. The user approves items one at a time.
15. Effort levels: PM and architect run at high, planners and implementers at
    medium. (`ORG.md` §7)
16. Commit `plans/`. The PM has standing permission to commit `plans/`-only
    snapshots to `dev` at gates and after each recorded decision.
    (`ORG.md` §5)

## Wave 1 decisions (2026-10-24)

- REQ-024: option 1: C1 makes the 7 call-site edits itself in one separate commit; architect checks the out-of-ownership diff at merge.
- REQ-042: approve C1 plan, conditional on the architect's 4 editorial fixes (state/architect.md, Plan reviews: C1) applied by the planner before dispatch.
- REQ-031: option 1: add riff_fixup.rs pre-pass, with the architect's 4 conditions.
- REQ-032: option 1: add symphonia `id3v2` feature (architect edits Cargo.toml on integrate/wave-1 and README §1.1).
- REQ-033: option 1: end-trim only for AAC; architect amends README §3.2 wording as in its review.
- REQ-034: option 1: substitute silence, fail file above 10% bad packets, with the architect's silence-sizing fix (capped).
- REQ-036: option 1: extension whitelist via `is_supported_extension`; architect amends README §3.2 wording.
- REQ-043: approve F1a plan, conditional on the architect's fixes (silence sizing, riff_fixup trailing-junk test, Path::new).
- REQ-044: approve F1b plan, conditional on the architect's editorial fixes.

## Organisation change (2026-10-24)

- Reduce to two agents to cut usage: architect + PM. The architect writes all
  remaining plans and plan fixes; the PM implements approved plans itself, one
  task at a time, in worktrees; the architect reviews and merges. Planners,
  implementers and design subagents are retired. (`ORG.md` top note; user via
  phone, options "architect creates all plans" and "A: PM implements".)
