---
id: REQ-037
type: question
from: planner (PB-audio-files / LOAD)
tasks: [LOAD, TRK]
blocks: [LOAD]
status: open
created: 2026-10-24
---

## Request
User decision (README §4 item 7): how are tracks named when the assets
directory has subfolders? The planner proposes a scheme.

## Context
LOAD makes `scan_assets` recursive (`detailed/LOAD.md` steps 3-4). Today a
track is named by its file stem, and `.` already means `group.voice` in
commands. The SH grammar treats `/`, `-`, `_` and `:` as ordinary word
characters (`detailed/SH-grammar.md` §1), so they need no quoting.
Voices are named after their track by default, so the track name also shows
up as a voice name (`load drums/kick` creates voice `drums/kick`, and inside
a group `g.drums/kick`).

## Options
1. **Relative path with `/`**: `assets/drums/909/kick.wav` → `drums/909/kick`;
   top-level files keep their bare stem (`kick`), so flat folders behave
   exactly as today. Clashes only arise from the same stem with different
   extensions in one folder (first in sort order wins, as today). Hidden
   entries (leading `.`) are skipped; depth limit 8. Readable, unambiguous,
   tab-completion friendly (INS can complete by prefix). (Recommended)
2. **Stem only, everywhere**: `kick` wherever it lives. Shortest names, but
   `drums/kick.wav` and `fx/kick.wav` clash and one is skipped.
3. **Path joined with `_` or `-`**: `drums_kick`. No new character, but
   ambiguous with real underscores (`drums_kick.wav` at the top level
   clashes with `drums/kick.wav`).

Sub-question (applies to any option): track names may contain `.` today
(`my.loop.wav` → `my.loop`), which collides with `group.voice` when the
voice takes the track's name. Recommendation: leave stems as they are now
(no behaviour change in LOAD) and let the PH/VOX naming rules decide how a
voice name with dots is resolved; TRK's `import … as <name>` rejects dots.
Alternative: LOAD replaces `.` with `_` in all track names.

## Impact
LOAD `asset_name` and `scan_assets` (steps 3-4) and their tests; TRK
`rescan` (uses the same names); INS completion (prefix matching works with
option 1); SH needs no change. Option 2 silently drops files in nested
libraries.

## Architect review

## Decision
