//! The card (REQ-056 §10): the page `blast --card` prints for an agent. The
//! prose is fixed; the kinds, settings and actions are generated from the
//! schemas, and the examples are the table below.

use super::help::{built_settings, kinds, relations};
use super::schema::{ACTIONS, Positional, kind_word, schema};

/// One example: a title and its statements (a block counts as one).
pub struct Example {
    pub title: &'static str,
    pub stmts: &'static [&'static str],
}

/// REQ-056 §8's ten pieces. Features that aren't built yet answer E404.
pub const EXAMPLES: &[Example] = &[
    Example {
        title: "Four on the floor at 124.",
        stmts: &[
            "clock main 124b",
            "voice kick from \"kick\"",
            "kick { seq pat 1 2 3 4 in 4 | trig }",
            "start kick",
        ],
    },
    Example {
        title: "Hats between the kicks; the last one only sometimes.",
        stmts: &[
            "voice hat from \"hat\" gain=0.4",
            "hat { seq off 1.5 2.5 3.5 4.5%40 in 4 | trig }",
        ],
    },
    Example {
        title: "Kick and hat in a group, started together.",
        stmts: &["group drums { kick; hat }", "start drums"],
    },
    Example {
        title: "Hats at 93 against the kick at 124.",
        stmts: &["hat on 93b"],
    },
    Example {
        title: "The kick's rhythm also plays the bass, on low notes.",
        stmts: &[
            "voice bass from \"bass\"",
            "kick.pat | trig, melo bass 36 36 43 41",
        ],
    },
    Example {
        title: "Two fills into the snare as one bundle.",
        stmts: &[
            "voice snare from \"snare\"",
            "packet fills {\n  seq a 4 4.5 in 4\n  seq b 2.75 4.25 in 8\n} | trig snare",
        ],
    },
    Example {
        title: "Everything up to 128, and the hat back onto main.",
        stmts: &["clock main 128b and hat on main"],
    },
    Example {
        title: "An organ from three sines.",
        stmts: &[
            "voice organ {\n  wave s1 shape=sine freq=220\n  wave s2 shape=sine freq=440\n  wave s3 shape=sine freq=880\n}",
            "organ.s2 freq=660",
        ],
    },
    Example {
        title: "Humanise the snare: lazy on the backbeat.",
        stmts: &["snare { seq back 2 4~0.05 late=0.03 in 4 | trig }"],
    },
    Example {
        title: "The lead on main's next 16; slow main on its next bar.",
        stmts: &["start lead at 16 by main", "clock main 120b at 4"],
    },
];

const PROSE: &str = "\
BLAST: live audio by statements about named things.
- Time is plural: clocks are objects (124b = 124 beats a minute, 500ms = 500 ms, 24000f
  = frames). Everything runs on a clock; no `on` means its parent's; the
  session's clock is `main`.
- Clocks change while playing: `clock main 124b` retimes without a jump;
  `hat on slow` / `on 93b` / `on kick` moves hat (and what follows it) to
  another clock; `remove slow` frees one nothing uses.
- Rhythm is positions on a clock: `seq pat 1 2 3 4 in 4` (1-based). A seq only
  bangs; `| trig` (restart a voice) or `| melo v 60 62` (pitch) says what it
  plays. `| trig drums` restarts a voice or a group's members; a pipe may list
  several (`| trig, melo bass 36 43`) and several seqs may share a target.
  `melo` sets the next note (60 = no shift) and restarts the voice. Restating
  only a pipe replaces it: `kick.pat | trig hat`. `packet fills { seq a 1;
  seq b 3 } | trig snare` shares one pipe (`start`, `pause`, `remove` and
  the like act on its seqs); a seq or packet outside any block needs a pipe
  and follows `main`.
- Statements create or update by name: `voice kick from \"kick\" rate=0.8`,
  later `kick rate=0.5`. Quoted = a file/track; bare words = your objects.
- Voice settings: `gain=0.5` or `gain=-6db`, `pan=-1..1`, `rate=-1` (back),
  `pitch=2` (pitch only: a ratio, 1 = normal, 0.5 = an octave down; also
  `7st`, `50c`, `1.5x`),
  `stretch=2` (speed only), `start= end= loopstart= loopend=` (0..1 of the
  track), `loop=forward|pingpong|off`, `slices=4 slice=2`
  (from 1). `bounce=2ms` fades a ping-pong turnaround to stop the click
  (0 = off). `kick from \"snare\"` swaps a voice's track.
- Tracks: the assets folder's files are tracks at startup. `track \"bass\" from
  \"~/s/bass.wav\"` imports one (a relative path starts in the assets folder;
  raw PCM takes `format=s16le rate=44100 channels=2`); restating it with another
  file swaps the samples under every voice playing it. `rescan` imports files
  new in the assets folder; `remove \"bass\"` drops a track no voice uses.
- Choirs: `choir=4` sings with four instances; `choirmode=seq` (a round,
  each step takes the next) or `choirmode=sim` (each step starts all);
  `intonation=0.8` (1 = exact) lets each instance drift in pitch, pan, level
  and onset.
- Blocks mean \"belongs to\": `kick { seq pat 1 2 3 4 }` makes kick.pat;
  `group drums { kick; hat }` puts voices in a group (restate a voice to move
  it, `ungroup drums.hat` takes it out, `drums gain=0.5 over 2` fades it).
  A seq in a group's block (`group drums { kick; hat; seq extra 1 3 }`)
  retriggers every member.
- Seqs edit in place: `kick.pat 1 3` replaces the steps without a restart,
  `kick.pat seed=7` makes chance repeat, `pause kick.pat` / `resume
  kick.pat` stop and restart its firing (the voice plays out), `stop` /
  `start kick.pat` also rewind it, `remove kick.pat` deletes it.
- Chance and feel: `3%50` plays a step half the time; `jitter=0.03`,
  `late=0.03`, `early=0.03` and `chance=70` set them for a whole seq (also
  `10ms`); `1~0.05` overrides one step. Every setting has a one-letter form:
  `kick r=1.5`.
- Glides: `over D` after a voice setting moves it there gradually:
  `snare r=2 over 1`, `snare pitch=2 over 1 gain=0.5 over 3b` (a bare number
  is periods, `b` beats, or `250ms`). Without `over` a change is instant.
Statement: [kind] name [from \"track\"] [on clock] [in N (cycle/period, beats)]
  [setting=value...] [{...}] [| receiver, ...] [at N | after N [by clock]]
Join with `and` to apply in the same instant. End with newline or `;`.
Waiting: `at N` is the next multiple of N beats of the first target's clock,
  `after N` is N beats on; `by clock` counts on another. Creating or removing
  can't wait on an `at`/`after` line (E410), but can after `then`, `wait` or in a
  script after a `wait` (voices, groups, clocks, seqs, lfos, envs, effects; not
  buffers, tracks or removing an effect). Such an object is not in `state` until
  it is made, and a line typed now can't name it before (E411). An object whose
  removal waits stays nameable for a line typed now until the removal runs: a
  setting on the glide `thenif` watches cuts the removal (W105), `remove` removes
  it now (W103), and its name is taken (E302).
Sequence: `a over 2b then b` runs b once a's longest glide is over, whatever
  happened to it; `thenif` runs it only if no glide was cut short (a retarget
  drops the rest, W105); `wait 4b then b` waits 4 beats of main first (`by clock`
  for another; `500ms` is clock time). `and` binds tighter than `then`.
Timed scripts: a line `wait 4b` of its own makes the lines after it run 4 beats
  later; waits add up and the script runs in the background. `end` stops what
  the script started and cancels the rest; `end after 16b` waits first;
  `end NAME` and `source NAME stop` (at the prompt) end a running script, the
  second without stopping anything. `state` lists running scripts.
";

const TAIL: &str = "\
Script: let x = 3; $x; (expr); source NAME [read]. Queries: state [path] [to
  NAME], check NAME, help [kind], quit (or exit). NAME is a file in the sources folder.
Durations: 0.5 = half the period; 2b beats; 5ms, 1.5s, 480f.
Errors: [file:]line:col: CODE message (suggestion). `state` output is valid
  input. Something not built yet answers E404 and names its task.
";

/// The kinds that work today, each with what it takes.
fn kinds_text() -> String {
    let mut items: Vec<String> = Vec::new();
    for kind in kinds() {
        let sc = schema(kind);
        if sc.built.is_some() {
            continue;
        }
        let mut parts: Vec<&str> = relations(sc);
        match sc.positional {
            Positional::No => {}
            Positional::Clock => parts.push("rate"),
            Positional::Steps => parts.push("steps"),
        }
        let mut item = format!("{} ({}", kind_word(kind), parts.join(", "));
        let settings: Vec<&str> = built_settings(sc).map(|s| s.name).collect();
        if !settings.is_empty() {
            item.push_str(&format!("; {}", settings.join(" ")));
        }
        item.push(')');
        items.push(item);
    }
    wrap("Kinds: ", &items.join(", "))
}

fn actions_text() -> String {
    let words: Vec<&str> = ACTIONS
        .iter()
        .filter(|a| a.built.is_none())
        .map(|a| a.word)
        .collect();
    format!("Actions: {}; `all` = every one.\n", words.join(" "))
}

/// Wraps `body` after `head` at about 76 columns, indenting continuations.
fn wrap(head: &str, body: &str) -> String {
    let mut out = String::from(head);
    let mut width = head.len();
    for word in body.split(' ') {
        if width + word.len() + 1 > 76 {
            out.truncate(out.trim_end().len());
            out.push_str("\n  ");
            width = 2;
        }
        out.push_str(word);
        out.push(' ');
        width += word.len() + 1;
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

/// The whole card.
pub fn card() -> String {
    let mut out = String::from(PROSE);
    out.push_str(&kinds_text());
    out.push_str(&actions_text());
    out.push_str(TAIL);
    out.push_str("Examples:\n");
    for (i, ex) in EXAMPLES.iter().enumerate() {
        out.push_str(&format!("# {}. {}\n", i + 1, ex.title));
        for s in ex.stmts {
            out.push_str(s);
            out.push('\n');
        }
    }
    out
}
