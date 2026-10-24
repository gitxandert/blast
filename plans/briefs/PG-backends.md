# PG: macOS and Windows backends (MAC, WIN)

**OBSOLETE.** cpal (README §4 item 10) supplies macOS and Windows output, and crossterm supplies their terminals. What's left for those platforms is testing, which the user deferred. Don't deploy this brief. It's kept only for the verification notes below.

Original note: The dev machine is Linux only, so
neither backend can be run here. Plan them anyway, and be explicit about what
can be verified:
- WIN: `rustup target add x86_64-pc-windows-gnu`, then
  `cargo check --target x86_64-pc-windows-gnu` (and, if mingw is installed, a
  build). Running it needs a Windows machine or Wine; Wine's WASAPI is partial.
- MAC: not even `cargo check` works without the Apple SDK. The plan must say
  how to keep the code honest anyway: careful FFI declarations with the header
  they mirror cited, and a manual test checklist for the user.

Both build against R1's `AudioBackend` trait and `render` contract (README §3.5),
with no crates and hand-written FFI. Each backend states how it reports the
actual device rate and channel count back through `config()`.

## MAC: coreaudio-backend (wave 2)

`runtime/backend/coreaudio.rs`: an AudioUnit (`kAudioUnitSubType_DefaultOutput`)
or AudioQueue render callback, chosen with a justification. Link
`AudioToolbox`/`CoreAudio` with `#[link(name = …, kind = "framework")]`. The
callback runs on Apple's real-time thread, so the README §1 rules apply inside
the callback. The `run(stop, render)` trait shape assumes a blocking loop, but
CoreAudio is callback-driven. Plan the adaptation (e.g. `run` starts the unit,
then parks until `stop` is set), and if the trait shape is wrong, raise it under
"Contract issues". The macOS terminal reuses `terminal/unix.rs`.

## WIN: wasapi-backend (wave 2)

`runtime/backend/wasapi.rs`: shared-mode, event-driven WASAPI (`IMMDeviceEnumerator` →
`IAudioClient` → `IAudioRenderClient`), with COM vtables declared by hand.
Handle the mix format (usually f32 at the device rate).
`runtime/terminal/windows.rs`: raw console mode
(`SetConsoleMode` with `ENABLE_VIRTUAL_TERMINAL_INPUT`/`PROCESSING`), reading
bytes so the existing `parse_key` keeps working, plus Ctrl-C handling. Also list
anything else in the crate that's Unix-only (`libc::raise(SIGTERM)` in
`Conductor::apply` for `Quit`, signal handlers in runtime) and propose
cfg-gated replacements. Those replacements are registry entries or follow-ups,
because WIN doesn't own those files.
