---
id: REQ-036
type: plan-change
from: planner (PB-audio-files / F1a)
tasks: [F1a, F1b, LOAD, TRK]
blocks: []
status: approved
created: 2026-10-24
---

## Request
`decode_file` sends a file to symphonia only if its extension is on a
whitelist; anything else is `UnsupportedFormat` without being read. README
§3.2 says "to symphonia otherwise".

## Context
Symphonia's probe scans up to 1 MB for any format's marker. Fed a stray
`.png`, `.txt` or `.DS_Store` from an assets folder, it can "find" MP3 frame
syncs in arbitrary bytes and return noise as audio. Today's `load_assets`
already filters by extension, so a whitelist keeps current behaviour and
spares reading non-audio files.

Whitelist (F1a + F1b): `wav wave aif aiff aifc flac mp1 mp2 mp3 m4a mp4 aac
adts au snd raw pcm`, case-insensitive. Exposed as
`file_parsing::is_supported_extension(ext)`, used by LOAD's `scan_assets`
and TRK's `rescan`. AU is still detected by magic regardless of extension
among whitelisted files.

## Options
1. **Whitelist** as above. (Recommended)
2. **Probe everything** as README §3.2 reads literally: no extension needed,
   but junk files may load as noise and every file in the folder is read.

## Impact
README §3.2 wording (architect). F1a step 3, F1b step 4, LOAD `scan_assets`,
TRK `rescan`. Files with unusual extensions (e.g. `.m4b`, `.wv`) need
renaming or a whitelist entry.

## Architect review
**Architect, 2026-10-24: option 1. Recommended.**

Today's `load_assets` already filters by extension, so a whitelist keeps
current behaviour, and it stops symphonia's 1 MB sync scan from turning junk
into noise. AU magic still wins among whitelisted names (F1b step 4), so
README §3.2's "by magic number or extension" still holds for raw/AU.

This narrows the literal §3.2 text ("to symphonia otherwise"), so it needs
the user's approval. If approved, I amend §3.2 to: "`decode_file` … dispatches
to `raw`/`au` by magic number or extension, and to symphonia for the other
supported extensions (`is_supported_extension`). Any other extension is
`UnsupportedFormat` without being read."

LOAD (`scan_assets`) and TRK (`rescan`) must use `is_supported_extension`,
not their own lists. I'll check that when their plans arrive.

## Decision

User, 2026-10-24 (via phone): **approved**, option 1: extension whitelist via `is_supported_extension`; architect amends README §3.2 wording.
