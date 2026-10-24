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
