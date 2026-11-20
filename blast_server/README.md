# blast server

The BLAST REPL in a browser, built for testing from a phone over Tailscale.
`blast server ARGS` runs this crate's binary, `blast-server ARGS`; it is a
separate binary so the web stack (axum, tokio, serde) stays out of blast's
own build and tests. It links `blast` as a library: the same
`CmdProcessor`, completion, history and engine the terminal REPL uses.

What it does:

- **The REPL**: every line goes through the same `CmdProcessor::submit` the
  terminal uses, so the language, blocks (`kick {` … `}`), `source`,
  `check`, `state`, `state to`, `help`, `clear`, `rescan`, buffers with
  `to=`, and error messages with suggestions behave exactly as in the
  terminal. Tab completion is the terminal's (`CmdProcessor::completions`),
  with candidates also shown as tappable chips while typing. Up/Down walk
  the history (shared with the terminal REPL's file by default), Ctrl-R
  searches it, Ctrl-C drops an open block. A symbol bar puts `{ } | = "`
  and friends one tap away on a phone keyboard.
- **Hearing it in the browser**: the engine's output is tapped and streamed
  over the same WebSocket (16-bit PCM, 20 ms chunks) to a jitter-buffered
  player, so the phone hears everything the engine plays. iOS plays through
  the silent switch. The Buffer setting is where the jitter buffer starts:
  each underrun grows it by half (up to 1 s) instead of letting the network
  chop the rhythm, and every cut is faded, so a stall is a short dip, not a
  click. Listening again (or moving the slider) starts over from the
  setting.
- **Output devices from the browser**: switch the engine between ALSA
  devices (built-in, USB, …) live, without restarting the session, or to
  "No local output", where a software clock drives the engine and the
  browser is the only listener. With PipeWire, the sinks (speakers, HDMI,
  USB interfaces, Bluetooth) are listed too, and blast's stream moves
  between them without touching the system default. A device that can't
  run at the session's rate can be picked anyway: the engine restarts at
  the device's rate and the session is rebuilt from its `state`. If the
  device disappears (USB unplugged), the engine keeps running on the
  software clock.
  The short list leads with the sound server (`alsa:default`,
  `alsa:pipewire`, `alsa:pulse`, `alsa:jack`); raw ALSA cards are listed
  only when the host has no usable sound server, and "show every ALSA
  device" lists them all, each labelled "direct (bypasses the sound
  server)". Prefer the sound server and pick a
  USB interface as a PipeWire sink: a direct ALSA device takes the card
  exclusively, and on snd-usb-audio a direct 128- or 256-frame two-period
  ring can run about 25% slow with no xrun. For direct use, set
  `options snd_usb_audio lowlatency=0` (machine-wide, needs root) or
  `period_frames = 512`. Through PipeWire, `period_frames` sets the
  quantum for the whole graph, so it affects other apps while blast runs.
- **Sources**: list, open, edit, check (against the live session, nothing
  applied), save, save & run (`source NAME`), load into history
  (`source NAME history`), run selected lines at the prompt, upload `.blast`
  files, and "Save session as…" (`state to NAME`).
- **Assets**: browse the assets folder with rate, channels and whether each
  file is loaded; preview any file on the phone (decoded by blast's own
  decoders, so raw PCM, ADPCM and AU preview too); insert a track name or a
  `voice NAME from "track"` line; upload audio (checked by blast's decoder
  before it lands), then `rescan`.
- **Per-user settings**: theme, font size, jitter buffer, volume, live
  completion, symbol keys… layered from built-ins, the config's
  `[defaults]`, `[users."login"]`, and what each user saves in the browser.

## Run it

```sh
cargo build --release          # builds blast and blast-server
blast server                   # foreground, http://127.0.0.1:8800/
blast server --bg              # background; prints the URL, pid and log
blast server status            # exit 0 when running, 3 when not
blast server stop
```

`--bg` starts the server again detached (its own process group, stdin
closed, output appended to `server.log_file`), waits until it is listening,
and returns. A detached server ignores SIGHUP. The running server keeps its
pid and URLs in `server.pid_file`; a second start is refused while it runs.

### Options and the config file

Every option except `--bg` (a flag only) has a key in the config file,
and the option wins:

| Option | Key | Default |
|---|---|---|
| `--config PATH` | (the file itself) | `~/.config/blast/server.toml` |
| `--host ADDR` | `server.host` | `127.0.0.1` |
| `--port N` | `server.port` | `8800` |
| `--log FILE` | `server.log_file` (where `--bg` logs) | `~/.local/state/blast/server/server.log` |
| `--web-root DIR` | `server.web_root` | `""` (the app built into the binary) |
| `--device ID` | `engine.device` | `default` (`none`: browsers only) |
| `--assets DIR` | `engine.assets_dir` | blast's `assets/` |
| `--sources DIR` | `engine.sources_dir` | blast's `sources/` |
| `--script NAME` | `engine.script` | `""` (none) |
| `--list-devices` | | print the output devices and exit |

When the config file doesn't exist, the server writes it with every key at
its default and a comment on each (`config::default_file`; a test checks
that it states every key). `$XDG_CONFIG_HOME` and `$XDG_STATE_HOME` move
the config and state folders. Relative paths in the file are taken from
the file's folder; relative paths on the command line from the working
directory. Other keys:

| Section | Holds |
|---|---|
| `[server]` | also `pid_file`, `state_dir`, `allowed_users`, `also_bind`, `max_upload_mb`, `scrollback` |
| `[engine]` | also `history` (`blast` / `per-user` / `off`), `fallback_to_none`, `remember_output`, `pipewire_routing` |
| `[stream]` | `format` (`i16`/`f32`), `chunk_ms`, `max_listeners` |
| `[defaults]` | preferences every user starts from |
| `[users."login"]` | `display_name` and preference overrides per Tailscale login (`local` for loopback) |

Browser-saved preferences go to `<state_dir>/users/<login>.toml` and win
over the config file.

### As a service

`systemd/blast-server.service` is a user unit (edit the binary's path):

```sh
cp blast_server/systemd/blast-server.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now blast-server
journalctl --user -u blast-server -f
```

## How it's built

```
src/
  main.rs            CLI, commands, listeners, shutdown
  daemon.rs          --bg (detached re-exec), the pid file, stop, status
  config.rs          the config file, preference layers, per-user files
  engine/
    mod.rs           Engine: boots sessions (assets → Conductor + CmdProcessor),
                     restarts (keeping `state`), `quit` → fresh session;
                     a supervisor thread serializes lifecycle changes
    session.rs       the session thread: the terminal Repl's job (submit,
                     history blocks, completions, engine events, buffer saves)
    output.rs        where audio goes: the Conductor sits in a Renderer behind
                     a mutex that outlives any one cpal stream, so outputs swap
                     live; the callback only try_locks. Software clock for
                     "none". Every block is also tapped (stereo) into a ring.
    stream.rs        the tap's reader: chunks → binary WebSocket messages
    hub.rs           JSON events to every browser, plus scrollback
    pipewire.rs      sinks via pw-dump; moves our stream with pw-metadata
  server/
    mod.rs           routes, user identity (headers from loopback, whois from
                     the tailnet), engine/device/prefs endpoints
    ws.rs            the REPL socket (submit, cancel, complete, history, listen)
    files.rs         sources and assets (path-checked), previews, uploads, the app
                     (built into the binary unless server.web_root is set)
web/                 no build step: index.html, app.js, audio.js (player),
                     ring.js (jitter buffer), worklet.js, style.css
```

Everything is shared: one engine, one transcript (every connected browser
sees every line, tagged with who typed it), like a terminal with several
keyboards. The session renders stereo; outputs map it onto the device's
channels.

### The WebSocket

Browser → server: `{"t":"submit","line"}`, `{"t":"cancel"}`,
`{"t":"complete","id","line","cursor"}` (cursor is a UTF-8 byte offset),
`{"t":"history"}`, `{"t":"listen","on"}`. Server → browser: `hello`,
`scrollback`, `print` (`kind`: echo/out/err/warn/info), `prompt`
(`pending` while a block is open), `clear`, `history`, `completions`,
`engine`, `status`, `listen`, and binary audio: a 12-byte header (`1`,
format, channels, rate, sequence) then interleaved samples.

## Worth moving into blast later

- `output.rs`'s swappable Renderer + tap: a `runtime::audio` that can
  change device mid-session would serve the terminal too
  (`device NAME` as a command).
- `session.rs` mirrors `Repl::run`'s event drain and `Keys::entry`'s
  block-history rule; a shared `runtime::Frontend` trait would remove the
  copy (agent.rs already notes a socket front end could reuse it).
- `EngineState`'s stdout notices (`describe`) are already strings, so
  nothing else needs changing.
