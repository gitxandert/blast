//! What each kind takes (REQ-056 §3): one table per kind, checked against a
//! parsed statement. The parser is loose on purpose; the errors for a wrong
//! setting, type or range come from here, where the kind is known.

use super::ast::*;
use super::errors::{Code, LangErr, Span};
use crate::audio_processing::engine::fx::spec;

/// The type and range of a setting's value.
#[derive(Clone, Copy, Debug)]
pub enum SettingType {
    Number {
        min: f64,
        max: f64,
    },
    Int {
        min: i64,
        max: i64,
    },
    Word(&'static [&'static str]),
    /// A bare number (fraction of the period) or a suffixed duration.
    Duration,
    /// A duration, or one of these words (`hold=retrigger`).
    DurationOr(&'static [&'static str]),
    Clock,
    /// A bare number is ticks; `b` is ticks too; `s`, `ms`, `us`, `ns` and
    /// `f` are converted with the object's clock.
    Ticks,
    /// The speed of an lfo: a bare number (cycle length in periods), `b`,
    /// `s`, `ms`, `us`, `ns` or `f` (a cycle length), or `hz` (cycles a
    /// second, 0.001..=200).
    Rate,
    /// A frequency: a number or `220hz` (in Hz), or a note name (`a3`,
    /// `c#4`, `eb2`; 12-EDO, A4 = 440 Hz). Checked against `min..=max` Hz.
    Freq {
        min: f64,
        max: f64,
    },
    /// A number in range, or one of these words (`ratio=none`).
    NumberOr {
        min: f64,
        max: f64,
        words: &'static [&'static str],
    },
    /// `yes` or `no`.
    Bool,
    /// A quoted string.
    Str,
}

/// One `name=value` setting of a kind.
#[derive(Clone, Copy, Debug)]
pub struct SettingSpec {
    pub name: &'static str,
    /// The one-letter form (`r` for `rate`), unique within the kind.
    pub short: char,
    pub ty: SettingType,
    pub default: &'static str,
    pub doc: &'static str,
    /// `None` = works; `None` = E404 naming the task that builds it.
    pub built: Option<&'static str>,
    /// Whether `over` may follow it (a glide, REQ-074).
    pub glides: bool,
    /// Whether it takes decibels (`-6db`) as well as a linear number.
    pub db: bool,
}

/// Whether a relation or positional item is allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Req {
    No,
    Optional,
    RequiredOnCreate,
}

/// The positional items a kind takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Positional {
    No,
    /// One clock literal (`120b`).
    Clock,
    /// Rhythm steps.
    Steps,
}

/// What a block after the statement holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockUse {
    No,
    /// Members (bare names), voice declarations and processes.
    Members,
    /// Processes owned by the object.
    Processes,
}

/// Everything the checker knows about one kind.
#[derive(Clone, Copy, Debug)]
pub struct KindSchema {
    pub kind: Kind,
    pub doc: &'static str,
    pub from: Req,
    pub on: Req,
    pub period: Req,
    pub positional: Positional,
    pub settings: &'static [SettingSpec],
    pub block: BlockUse,
    pub pipe: bool,
    /// `Some(task)`: the kind itself isn't built, so even creating one is E404.
    pub built: Option<&'static str>,
    /// `Some(task)`: restating the kind with different values isn't built.
    pub update_built: Option<&'static str>,
}

/// An action word (`start`, `remove`, …).
#[derive(Clone, Copy, Debug)]
pub struct ActionSpec {
    pub word: &'static str,
    pub takes: &'static str,
    pub doc: &'static str,
    /// `Some(task)` if the whole action is not built.
    pub built: Option<&'static str>,
}

/// Every action, for `help` (LANGd) and the card.
pub const ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        word: "start",
        takes: "voices, groups, clocks, seqs, `all`",
        doc: "plays from the top (a clock: everything on it)",
        built: None,
    },
    ActionSpec {
        word: "stop",
        takes: "voices, groups, clocks, seqs, `all`",
        doc: "stops and rewinds",
        built: None,
    },
    ActionSpec {
        word: "pause",
        takes: "voices, groups, clocks, seqs, `all`",
        doc: "holds in place",
        built: None,
    },
    ActionSpec {
        word: "resume",
        takes: "voices, groups, clocks, seqs, `all`",
        doc: "continues after pause",
        built: None,
    },
    ActionSpec {
        word: "remove",
        takes: "voices, groups, clocks, seqs, tracks (quoted), `all`",
        doc: "deletes an object (a seq too); a group goes with its members, a track must be unused, `all` removes everything but main",
        built: None,
    },
    ActionSpec {
        word: "ungroup",
        takes: "a group, or members, `all`",
        doc: "a member back to the session level; a group dissolves, keeping its members",
        built: None,
    },
    ActionSpec {
        word: "rescan",
        takes: "nothing",
        doc: "imports files new in the assets folder",
        built: None,
    },
];

pub const fn setting(
    name: &'static str,
    short: char,
    ty: SettingType,
    default: &'static str,
    doc: &'static str,
    built: Option<&'static str>,
) -> SettingSpec {
    SettingSpec {
        name,
        short,
        ty,
        default,
        doc,
        built,
        glides: false,
        db: false,
    }
}

impl SettingSpec {
    /// The same setting, marked as one that also takes decibels.
    pub const fn db(mut self) -> Self {
        self.db = true;
        self
    }

    /// The same setting, marked as one `over` may follow.
    pub const fn glides(mut self) -> Self {
        self.glides = true;
        self
    }
}

pub const UNIT: SettingType = SettingType::Number { min: 0.0, max: 1.0 };

const VOICE_SETTINGS: &[SettingSpec] = &[
    setting(
        "gain",
        'g',
        SettingType::Number { min: 0.0, max: 8.0 },
        "1",
        "linear gain, or decibels (-6db)",
        None,
    )
    .glides()
    .db(),
    setting(
        "pan",
        'p',
        SettingType::Number {
            min: -1.0,
            max: 1.0,
        },
        "0",
        "-1 left .. 1 right",
        None,
    )
    .glides(),
    setting(
        "rate",
        'r',
        SettingType::Number {
            min: -16.0,
            max: 16.0,
        },
        "1",
        "playback rate; negative plays backwards",
        None,
    )
    .glides(),
    setting(
        "pitch",
        't',
        SettingType::Number {
            min: -48.0,
            max: 48.0,
        },
        "1",
        "pitch only: a ratio (1 is normal, 0.5 an octave down, 2 an octave up; 1.5x too), semitones (7st) or cents (50c)",
        None,
    )
    .glides(),
    setting(
        "stretch",
        'x',
        SettingType::Number {
            min: 0.0625,
            max: 16.0,
        },
        "1",
        "speed only, without the pitch (2 = twice as fast)",
        None,
    )
    .glides(),
    setting(
        "start",
        's',
        UNIT,
        "0",
        "start point, 0..1 of the track",
        None,
    )
    .glides(),
    setting("end", 'e', UNIT, "1", "end point, 0..1 of the track", None).glides(),
    setting(
        "loop",
        'l',
        SettingType::Word(&["off", "forward", "pingpong"]),
        "off",
        "loop mode",
        None,
    ),
    setting("loopstart", 'a', UNIT, "0", "loop start, 0..1", None).glides(),
    setting("loopend", 'b', UNIT, "1", "loop end, 0..1", None).glides(),
    setting(
        "slices",
        'n',
        SettingType::Int { min: 0, max: 1024 },
        "0",
        "slice count (0 = off)",
        None,
    ),
    setting(
        "slice",
        'c',
        SettingType::Int { min: 1, max: 1024 },
        "1",
        "which slice, counted from 1",
        None,
    ),
    setting(
        "choir",
        'h',
        SettingType::Int { min: 1, max: 8 },
        "1",
        "instances of the voice (1..8)",
        None,
    ),
    setting(
        "choirmode",
        'm',
        SettingType::Word(&["seq", "sequential", "sim", "simultaneous"]),
        "seq",
        "seq: each step takes the next instance; sim: each step starts all",
        None,
    ),
    setting(
        "intonation",
        'i',
        UNIT,
        "1",
        "1 keeps instances exact; lower detunes, pans, softens and delays them",
        None,
    ),
    setting(
        "shift",
        'k',
        SettingType::Word(&["spectral", "grains"]),
        "grains",
        "pitch/stretch engine: grains (default, WSOLA) or spectral",
        None,
    ),
    setting(
        "resample",
        'z',
        SettingType::Word(&["iir", "cubic"]),
        "iir",
        "how a plain head reads the source: iir (band-limited at any rate, default) or cubic (Hermite; aliases above 1x). Not for pitch/stretch, whose grains and spectral shifter read the source themselves. A sounding note keeps its method until the next hit",
        None,
    ),
    setting(
        "window",
        'w',
        SettingType::Duration,
        "90ms",
        "spectral shifter window: shorter is crisper with less latency, longer keeps chords in tune (20ms..250ms); long windows cost more on each retrigger",
        None,
    ),
    setting(
        "bounce",
        'u',
        SettingType::Duration,
        "2ms",
        "fade at a ping-pong turnaround, to stop the click (0 = off; 0..5ms)",
        None,
    ),
];

const GROUP_SETTINGS: &[SettingSpec] = &[setting(
    "gain",
    'g',
    SettingType::Number { min: 0.0, max: 8.0 },
    "1",
    "scales the members' level; glides with `over`",
    None,
)
.glides()
.db()];

/// The sample formats of raw PCM (`file_parsing::raw::SampleEncoding`).
const RAW_FORMATS: &[&str] = &[
    "s8", "u8", "s16le", "s16be", "s24le", "s24be", "s32le", "s32be", "f32le", "f32be", "f64le",
    "f64be",
];

const TRACK_SETTINGS: &[SettingSpec] = &[
    setting(
        "format",
        'f',
        SettingType::Word(RAW_FORMATS),
        "",
        "raw PCM sample format",
        None,
    ),
    setting(
        "rate",
        'r',
        SettingType::Number {
            min: 1.0,
            max: 768_000.0,
        },
        "",
        "raw PCM sample rate",
        None,
    ),
    setting(
        "channels",
        'c',
        SettingType::Int { min: 1, max: 64 },
        "",
        "raw PCM channels",
        None,
    ),
];

const SEQ_SETTINGS: &[SettingSpec] = &[
    setting(
        "seed",
        's',
        SettingType::Int {
            min: 0,
            max: i64::MAX,
        },
        "random",
        "makes chance and jitter repeat",
        None,
    ),
    setting(
        "chance",
        'c',
        SettingType::Number {
            min: 0.0,
            max: 100.0,
        },
        "100",
        "percent chance of each step (a step's %n overrides it)",
        None,
    )
    .glides(),
    setting(
        "jitter",
        'j',
        SettingType::Ticks,
        "0",
        "each step lands early or late by up to this (a step's ~a overrides it)",
        None,
    )
    .glides(),
    setting(
        "late",
        'l',
        SettingType::Ticks,
        "0",
        "each step lands up to this late",
        None,
    )
    .glides(),
    setting(
        "early",
        'e',
        SettingType::Ticks,
        "0",
        "each step lands up to this early",
        None,
    )
    .glides(),
    setting(
        "steps",
        'n',
        SettingType::Int { min: 1, max: 1024 },
        "none",
        "generate this many steps per cycle, spread evenly over the grid (instead of written steps)",
        None,
    ),
    setting(
        "grid",
        'g',
        SettingType::Int { min: 1, max: 1024 },
        "the cycle's beats",
        "slots per cycle: with steps= the generator spreads over them; with written steps each number counts a slot (grid=16 in 4: step 5 is beat 2; fractions are fine) instead of a beat",
        None,
    ),
    setting(
        "rot",
        'r',
        SettingType::Int {
            min: -1024,
            max: 1024,
        },
        "0",
        "moves every step (generated or written) this many slots later, wrapping within the cycle; without grid= a slot is a beat. Marks (% chance, ~ jitter) and jitter/late/early stay in beats",
        None,
    ),
];

// the settings every effect shares
pub const FX_MIX: SettingSpec = setting("mix", 'm', UNIT, "1", "dry/wet, 0..1", None).glides();

pub const FX_GAIN: SettingSpec = setting(
    "gain",
    'g',
    SettingType::Number { min: 0.0, max: 8.0 },
    "1",
    "the effect's output level, linear or decibels (6db), applied last",
    None,
)
.glides()
.db();

static CLOCK: KindSchema = KindSchema {
    kind: Kind::Clock,
    doc: "a clock: 120b is 120 beats a minute; or milliseconds (500ms) or frames (24000f); change: clock NAME RATE [over D] (an accelerando); remove NAME",
    from: Req::No,
    on: Req::No,
    period: Req::No,
    positional: Positional::Clock,
    settings: &[],
    block: BlockUse::No,
    pipe: false,
    built: None,
    update_built: None,
};

static TRACK: KindSchema = KindSchema {
    kind: Kind::Track,
    doc: "an audio file imported as a track; restating one re-imports it",
    from: Req::RequiredOnCreate,
    on: Req::No,
    period: Req::No,
    positional: Positional::No,
    settings: TRACK_SETTINGS,
    block: BlockUse::No,
    pipe: false,
    built: None,
    update_built: None,
};

static VOICE: KindSchema = KindSchema {
    kind: Kind::Voice,
    doc: "plays a track, a buffer or waves (from is optional when its block holds a wave); `NAME on slow` / `on 93b` / `on kick` moves it (and what follows it) to another clock",
    // a voice needs a track, a buffer or a wave: `lower_voice` says which is
    // missing (E405), since the block decides
    from: Req::Optional,
    on: Req::Optional,
    period: Req::Optional,
    positional: Positional::No,
    settings: VOICE_SETTINGS,
    block: BlockUse::Processes,
    pipe: false,
    built: None,
    update_built: None,
};

static GROUP: KindSchema = KindSchema {
    kind: Kind::Group,
    doc: "voices that start, stop and share a clock together; `NAME on slow` moves the group and the members that follow it",
    from: Req::No,
    on: Req::Optional,
    period: Req::Optional,
    positional: Positional::No,
    settings: GROUP_SETTINGS,
    block: BlockUse::Members,
    pipe: false,
    built: None,
    update_built: None,
};

static SEQ: KindSchema = KindSchema {
    kind: Kind::Seq,
    doc: "rhythm positions that bang a receiver; restate `owner.NAME` with new steps, `in` or settings to edit it in place; `voice.NAME on slow` moves only the seq; pause, resume, start, stop and remove take its path",
    from: Req::No,
    on: Req::Optional,
    period: Req::Optional,
    positional: Positional::Steps,
    settings: SEQ_SETTINGS,
    block: BlockUse::No,
    pipe: true,
    built: None,
    update_built: None,
};

static PACKET: KindSchema = KindSchema {
    kind: Kind::Packet,
    doc: "seqs that share one pipe: packet NAME { seq a 1 2 in 4; seq b 3 4 in 8 } | trig drums; `on` and `in` are the defaults for its seqs; start, stop, pause, resume and remove act on all of them",
    from: Req::No,
    on: Req::Optional,
    period: Req::Optional,
    positional: Positional::No,
    settings: &[],
    block: BlockUse::Processes,
    pipe: true,
    built: None,
    update_built: None,
};

const LFO_SETTINGS: &[SettingSpec] = &[
    setting(
        "to",
        't',
        SettingType::Word(&[]),
        "(required)",
        "what it modulates: a voice parameter (gain, pan, rate, pitch, stretch, start, end, loopstart, loopend, slice, intonation) or an effect's, as FX.PARAM (lo.cutoff)",
        None,
    ),
    setting(
        "depth",
        'd',
        SettingType::Number {
            min: -1000.0,
            max: 1000.0,
        },
        "(required)",
        "peak deviation in the target's units (a cutoff: octaves; pitch: semitones, so 7, 7st or 50c)",
        None,
    )
    .glides(),
    setting(
        "freq",
        'f',
        SettingType::Rate,
        "1",
        "speed: 5hz is five cycles a second; any other value is how long one cycle lasts: a bare number in periods of the owner (0.5 is two a period), 4b in beats, 250ms, 2s or 4800f in time",
        None,
    )
    .glides(),
    setting(
        "shape",
        's',
        SettingType::Word(&["sine", "tri", "square", "saw", "sh"]),
        "sine",
        "the wave; saw rises, sh holds a random value per cycle",
        None,
    ),
    setting(
        "offset",
        'o',
        SettingType::Number {
            min: -1000.0,
            max: 1000.0,
        },
        "0",
        "added to the wave (depth=0.5 offset=0.5 swings 0..1)",
        None,
    )
    .glides(),
    setting(
        "phase",
        'p',
        UNIT,
        "0",
        "where in the cycle it starts, 0..1 (0.25 is a quarter in)",
        None,
    ),
    setting(
        "seed",
        'r',
        SettingType::Int {
            min: 0,
            max: i64::MAX,
        },
        "random",
        "makes sh repeat",
        None,
    ),
];

static LFO: KindSchema = KindSchema {
    kind: Kind::Lfo,
    doc: "a low-frequency oscillator owned by a voice or group: adds a wave to one parameter (the base value never changes); `owner.NAME depth=0.5 over 4` glides it; pause holds the value, stop takes the effect away, start runs it again from phase",
    from: Req::No,
    on: Req::No,
    period: Req::No,
    positional: Positional::No,
    settings: LFO_SETTINGS,
    block: BlockUse::No,
    pipe: false,
    built: None,
    update_built: None,
};
const ENV_SETTINGS: &[SettingSpec] = &[
    setting(
        "attack",
        'a',
        SettingType::Duration,
        "0",
        "the rise from the current level to 1: a bare number is a fraction of the owner's period; 5ms, 2b, 480f",
        None,
    ),
    setting(
        "decay",
        'd',
        SettingType::Duration,
        "0",
        "the fall from 1 to sustain",
        None,
    ),
    setting(
        "sustain",
        's',
        UNIT,
        "1",
        "the level held after the decay, 0..1",
        None,
    ),
    setting(
        "hold",
        'h',
        SettingType::DurationOr(&["retrigger"]),
        "retrigger",
        "how long the sustain lasts before the release; retrigger = until the next trigger (there is no note-off)",
        None,
    ),
    setting(
        "release",
        'r',
        SettingType::Duration,
        "0",
        "the fall from sustain to 0, after the hold",
        None,
    ),
    setting(
        "to",
        't',
        SettingType::Word(&[]),
        "gain",
        "what it shapes: gain (multiplies) or any parameter an lfo targets (adds depth times the level): pitch, lo.cutoff",
        None,
    ),
    setting(
        "depth",
        'p',
        SettingType::Number {
            min: -1000.0,
            max: 1000.0,
        },
        "1",
        "the amount: on gain 0..1 (1 is silence between envelopes); elsewhere added at full level (a cutoff: octaves; pitch: semitones, so 7, 7st or 50c)",
        None,
    ),
    setting(
        "curve",
        'c',
        SettingType::Word(&["lin", "exp"]),
        "exp",
        "how decay and release fall: exp is -60 dB at the stage's end; the attack is always linear",
        None,
    ),
];

static ENV: KindSchema = KindSchema {
    kind: Kind::Env,
    doc: "an envelope owned by a voice or group: fires every time its owner is triggered (a start, a seq step, a trig, a melo note) and shapes one parameter; `owner.NAME decay=400ms` changes it from the next trigger; pause holds the level, stop takes the effect away, start waits for the next trigger",
    from: Req::No,
    on: Req::No,
    period: Req::No,
    positional: Positional::No,
    settings: ENV_SETTINGS,
    block: BlockUse::No,
    pipe: false,
    built: None,
    update_built: None,
};
const WAVE_SETTINGS: &[SettingSpec] = &[
    setting(
        "shape",
        's',
        SettingType::Word(&["sine", "tri", "saw", "square", "noise", "table"]),
        "sine",
        "the waveform; saw and square are band-limited, noise is white; table plays single cycles cut from a track or buffer (from)",
        None,
    ),
    setting(
        "freq",
        'f',
        SettingType::Freq {
            min: 0.01,
            max: 20_000.0,
        },
        "220",
        "pitch in Hz (220, 220hz) or a note (a3, c#4, eb2; A4 is 440); the voice's pitch and rate multiply it",
        None,
    )
    .glides(),
    setting(
        "gain",
        'g',
        SettingType::Number { min: 0.0, max: 8.0 },
        "1",
        "linear gain, or decibels (-6db)",
        None,
    )
    .glides()
    .db(),
    setting(
        "phase",
        'p',
        UNIT,
        "0",
        "where in the cycle a retriggered wave starts, 0..1",
        None,
    ),
    setting(
        "pw",
        'w',
        SettingType::Number {
            min: 0.01,
            max: 0.99,
        },
        "0.5",
        "a square's pulse width (the share of the cycle it is high)",
        None,
    )
    .glides(),
    setting(
        "retrig",
        'r',
        SettingType::Bool,
        "yes",
        "yes: every trigger (a start, a seq step, a trig, a melo note) restarts the phase; no: it runs free",
        None,
    ),
    setting(
        "slices",
        'n',
        SettingType::Int { min: 1, max: 256 },
        "1",
        "a table wave: the source is cut into this many equal cycles (1..256); pos moves between them",
        None,
    ),
    setting(
        "frames",
        'k',
        SettingType::Int { min: 256, max: 4096 },
        "2048",
        "a table wave: samples in each cycle, a power of two (256..4096)",
        None,
    ),
    setting(
        "pos",
        'o',
        UNIT,
        "0",
        "a table wave: where it is among its cycles, 0 the first and 1 the last; between two it fades from one to the other",
        None,
    )
    .glides(),
    setting(
        "fm",
        'm',
        SettingType::Word(&[]),
        "none",
        "the wave(s) of this voice that phase-modulate this one: one wave, or several joined by + with no spaces (fm=a+b), or none; each modulator's gain sets its share, a modulator is computed before what it modulates, a loop is refused (a wave can't name itself: use fb); a noise wave ignores it",
        None,
    ),
    setting(
        "index",
        'i',
        SettingType::Number { min: 0.0, max: 32.0 },
        "1",
        "FM depth in radians: the modulator's output (its gain included) times this; 0 is the plain carrier",
        None,
    )
    .glides(),
    setting(
        "fb",
        'b',
        SettingType::Number { min: 0.0, max: 4.0 },
        "0",
        "feedback: the wave modulates itself, in radians (its last two samples, averaged, push its phase); 0 is none; a noise wave ignores it",
        None,
    )
    .glides(),
    setting(
        "ratio",
        'a',
        SettingType::NumberOr {
            min: 0.01,
            max: 64.0,
            words: &["none"],
        },
        "none",
        "the frequency as a multiple of the voice's note (220 Hz times its pitch), instead of freq; a fraction is kept as typed (ratio=3/2); 2 is an octave above whatever the voice plays; none goes back to freq",
        None,
    )
    .glides(),
    setting(
        "out",
        'u',
        SettingType::Bool,
        "yes",
        "no: the wave is not heard (a modulator only)",
        None,
    ),
];

static WAVE: KindSchema = KindSchema {
    kind: Kind::Wave,
    doc: "a generated sound owned by a voice (up to 8): `wave s1 shape=saw freq=a3` in its block, or `voice.NAME freq=330 over 2` to change it; a voice of waves needs no `from`; the voice's gain, pan, pitch, effects, lfos, envs and seqs all apply; stop silences it and restarts its phase, pause holds the phase; remove voice.NAME; `wave t shape=table from \"pad\" slices=8 pos=0.3` (or from BUFFER) plays cycles cut from a track or buffer",
    from: Req::Optional,
    on: Req::No,
    period: Req::No,
    positional: Positional::No,
    settings: WAVE_SETTINGS,
    block: BlockUse::No,
    pipe: false,
    built: None,
    update_built: None,
};
const BUFFER_SETTINGS: &[SettingSpec] = &[
    setting(
        "from",
        'f',
        SettingType::Word(&[]),
        "out",
        "what fills it: out (the whole mix before the output clamp), a voice (after its effects, pan and gain) or a group (its mix after effects and gain)",
        None,
    ),
    setting(
        "length",
        'l',
        SettingType::Duration,
        "30s",
        "its size: 30s, 500ms, 48000f; 4b (or a bare 4) counts beats of the `on` clock",
        None,
    ),
    setting(
        "loop",
        'o',
        SettingType::Bool,
        "no",
        "yes: when full, keep recording from the beginning until stop or loop=no",
        None,
    ),
    setting(
        "live",
        'v',
        SettingType::Bool,
        "no",
        "no: a take replaces what voices hear when it ends (twice the memory); yes: voices hear each frame as it is written",
        None,
    ),
    setting(
        "bits",
        'b',
        SettingType::Int { min: 8, max: 32 },
        "32",
        "8, 16, 24 or 32: lower is stored quantised with dither as it records",
        None,
    ),
    setting(
        "to",
        't',
        SettingType::Str,
        "none",
        "written when the take stops: \"name.wav\" (an extension or a /) is a file, overwritten; \"name\" makes or replaces a track; \"\" is nothing",
        None,
    ),
];

static BUFFER: KindSchema = KindSchema {
    kind: Kind::Buffer,
    doc: "memory that voices play (`voice re from NAME`) while it records; start, pause, resume and stop take a recording; `on CLOCK` is what 4b counts; remove NAME",
    from: Req::No,
    on: Req::Optional,
    period: Req::No,
    positional: Positional::No,
    settings: BUFFER_SETTINGS,
    block: BlockUse::No,
    pipe: false,
    built: None,
    update_built: None,
};

/// The schema of `kind`.
pub fn schema(kind: Kind) -> &'static KindSchema {
    match kind {
        Kind::Clock => &CLOCK,
        Kind::Track => &TRACK,
        Kind::Voice => &VOICE,
        Kind::Group => &GROUP,
        Kind::Seq => &SEQ,
        Kind::Packet => &PACKET,
        Kind::Lfo => &LFO,
        Kind::Env => &ENV,
        Kind::Fx(t) => &spec(t).schema,
        Kind::Wave => &WAVE,
        Kind::Buffer => &BUFFER,
    }
}

/// The word for a kind, as written.
pub fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::Clock => "clock",
        Kind::Track => "track",
        Kind::Voice => "voice",
        Kind::Group => "group",
        Kind::Seq => "seq",
        Kind::Packet => "packet",
        Kind::Lfo => "lfo",
        Kind::Env => "env",
        Kind::Fx(t) => spec(t).word,
        Kind::Wave => "wave",
        Kind::Buffer => "buffer",
    }
}

/// The word for an action, as written.
pub fn action_word(word: ActionWord) -> &'static str {
    match word {
        ActionWord::Start => "start",
        ActionWord::Stop => "stop",
        ActionWord::Pause => "pause",
        ActionWord::Resume => "resume",
        ActionWord::Remove => "remove",
        ActionWord::Ungroup => "ungroup",
        ActionWord::Rescan => "rescan",
    }
}

/// The spec of an action word.
pub fn action_spec(word: ActionWord) -> &'static ActionSpec {
    let w = action_word(word);
    // every word is in the table
    ACTIONS.iter().find(|a| a.word == w).unwrap_or(&ACTIONS[0])
}

/// E404: `what` exists in the grammar but its task hasn't landed.
pub fn not_built(span: Span, what: &str, task: &str) -> LangErr {
    LangErr::new(
        Code::NotBuiltYet,
        span,
        format!("{what} isn't built yet (task {task})"),
    )
}

/// The linear gain a decibel value means: `10^(db / 20)`.
pub fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// A pitch in semitones: `7st` is 7, `50c` is 0.5, `1.5x` (or a bare `1.5`) is
/// `12 log2 1.5`. A ratio must be above 0.
pub fn pitch_semitones(n: f64, unit: PitchUnit) -> Option<f64> {
    match unit {
        PitchUnit::Semitones => Some(n),
        PitchUnit::Cents => Some(n / 100.0),
        PitchUnit::Ratio if n > 0.0 => Some(12.0 * n.log2()),
        PitchUnit::Ratio => None,
    }
}

/// The value of a number: a literal, or an expression of literals. Variables
/// are replaced by their values before lowering (`vars::substitute`), so one
/// that is still here was never defined.
pub fn eval_num(n: &Num) -> Result<f64, LangErr> {
    match n {
        Num::Lit(v, _) => Ok(*v),
        Num::Var(id) => Err(unknown_var(id)),
        Num::Expr(e, span) => eval_expr(e, *span),
    }
}

fn unknown_var(id: &Ident) -> LangErr {
    LangErr::new(
        Code::UnknownName,
        id.span,
        format!("unknown variable '${}'", id.name),
    )
}

fn eval_expr(e: &Expr, span: Span) -> Result<f64, LangErr> {
    match e {
        Expr::Num(v) => Ok(*v),
        Expr::Var(id) => Err(unknown_var(id)),
        Expr::Neg(inner) => Ok(-eval_expr(inner, span)?),
        Expr::Bin(a, op, b) => {
            let (a, b) = (eval_expr(a, span)?, eval_expr(b, span)?);
            Ok(match op {
                BinOp::Add => a + b,
                BinOp::Sub => a - b,
                BinOp::Mul => a * b,
                BinOp::Div if b == 0.0 => {
                    return Err(LangErr::new(Code::OutOfRange, span, "division by zero"));
                }
                BinOp::Div => a / b,
            })
        }
    }
}

/// A checked setting.
#[derive(Clone, Copy, Debug)]
pub struct CheckedSetting<'a> {
    pub spec: &'static SettingSpec,
    pub value: &'a Value,
    /// The `over` that followed it, if any.
    pub over: Option<&'a Over>,
    pub span: Span,
}

/// A statement's items, checked against a schema.
#[derive(Debug, Default)]
pub struct Checked<'a> {
    pub from: Option<(&'a str, Span)>,
    /// `from NAME`: a buffer (RECb).
    pub from_name: Option<&'a Ident>,
    pub on: Option<&'a ClockRef>,
    /// `in N`, in ticks.
    pub period: Option<(u32, Span)>,
    pub clock: Option<&'a ClockLit>,
    /// The steps as written, in order (a seq).
    pub steps: Vec<&'a Step>,
    pub settings: Vec<CheckedSetting<'a>>,
}

impl<'a> Checked<'a> {
    /// The value of the setting called `name`, if given.
    pub fn setting(&self, name: &str) -> Option<&CheckedSetting<'a>> {
        self.settings.iter().find(|s| s.spec.name == name)
    }
}

// what to say about a setting that used to exist
fn removed_setting_hint(kind: Kind, name: &str) -> Option<&'static str> {
    match (kind, name) {
        (Kind::Voice, "reverse") => Some("direction is the sign of rate: rate=-1 plays backwards"),
        (Kind::Voice, "fadein" | "fadeout") => {
            Some("a fade is a gain glide: gain=0 over 1b, gain=1 over 1b")
        }
        _ => None,
    }
}

// `over D` takes a bare number (periods) or a duration, and not a negative one
fn check_over(o: &Over) -> Result<(), LangErr> {
    let (Value::Num(n) | Value::Duration(n, _)) = &o.value else {
        return Err(wrong_type(o.span, "over", "a duration (1, 2b, 250ms)"));
    };
    if matches!(o.value, Value::Duration(_, DurUnit::Hz)) {
        return Err(hz_is_a_rate(o.span));
    }
    if eval_num(n)? < 0.0 {
        return Err(LangErr::new(
            Code::OutOfRange,
            o.span,
            "over can't be negative",
        ));
    }
    Ok(())
}

/// What an lfo says to `in` and `on`.
const LFO_SPEED_HINT: &str = "lfo speed is freq=: 4b, 2, 250ms or 5hz";

/// E402: `hz` where a duration is wanted.
pub fn hz_is_a_rate(span: Span) -> LangErr {
    LangErr::new(
        Code::WrongType,
        span,
        "hz is a rate; this setting takes a duration",
    )
}

fn wrong_type(span: Span, what: &str, want: &str) -> LangErr {
    LangErr::new(Code::WrongType, span, format!("{what} takes {want}"))
}

fn value_span(v: &Value) -> Option<Span> {
    match v {
        Value::Num(n)
        | Value::Typed(n, _)
        | Value::Duration(n, _)
        | Value::Db(n)
        | Value::Pitch(n, _) => Some(n.span()),
        Value::Word(id) => Some(id.span),
        Value::Words(w) => w.first().map(|f| f.span.to(w[w.len() - 1].span)),
        Value::Str(_) => None,
    }
}

/// Checks one value against a setting's type.
fn check_value(spec: &SettingSpec, value: &Value, at: Span) -> Result<(), LangErr> {
    if let Value::Typed(n, _) = value {
        return check_value(spec, &Value::Num(n.clone()), at);
    }
    let span = value_span(value).unwrap_or(at);
    let name = spec.name;
    // a bare number on `pitch` is a ratio
    let pitch = match value {
        Value::Num(n) if name == "pitch" => Some((n, PitchUnit::Ratio)),
        Value::Pitch(n, unit) => Some((n, *unit)),
        _ => None,
    };
    if let Some((n, unit)) = pitch {
        // an lfo's or env's `depth` on pitch is semitones (7st, 50c)
        let ok = name == "pitch" || (name == "depth" && unit != PitchUnit::Ratio);
        if !ok {
            let want = match name {
                "depth" => "a number, or semitones (7st) or cents (50c) on pitch",
                _ => "a number",
            };
            return Err(wrong_type(span, name, want));
        }
        let SettingType::Number { min, max } = spec.ty else {
            return Err(wrong_type(span, name, "a number"));
        };
        let Some(v) = pitch_semitones(eval_num(n)?, unit) else {
            return Err(
                LangErr::new(Code::OutOfRange, span, "a pitch ratio must be above 0")
                    .with_hint("1 is normal, 0.5 an octave down, 2 an octave up"),
            );
        };
        if v < min || v > max {
            return Err(LangErr::new(
                Code::OutOfRange,
                span,
                format!("{name} must be between {min} and {max} semitones, not {v}"),
            ));
        }
        return Ok(());
    }
    if matches!(value, Value::Duration(_, DurUnit::Hz))
        && matches!(
            spec.ty,
            SettingType::Duration
                | SettingType::DurationOr(_)
                | SettingType::Ticks
                | SettingType::Clock
        )
    {
        return Err(hz_is_a_rate(span));
    }
    match (spec.ty, value) {
        (SettingType::Word([]), Value::Word(_)) => Ok(()),
        (SettingType::Rate, Value::Duration(n, DurUnit::Hz)) => {
            let v = eval_num(n)?;
            if !(0.001..=200.0).contains(&v) {
                return Err(LangErr::new(
                    Code::OutOfRange,
                    span,
                    format!("{name} must be between 0.001hz and 200hz, not {v}hz"),
                ));
            }
            Ok(())
        }
        (SettingType::Rate, Value::Num(n) | Value::Duration(n, _)) => {
            let v = eval_num(n)?;
            if !(v > 0.0 && v.is_finite()) {
                return Err(LangErr::new(
                    Code::OutOfRange,
                    span,
                    format!("{name} must be above 0, not {v}"),
                ));
            }
            Ok(())
        }
        (SettingType::Freq { min, max }, v) => {
            let hz = match v {
                Value::Num(n) | Value::Duration(n, DurUnit::Hz) => eval_num(n)?,
                Value::Word(w) => super::lower::note_hz(&w.name).ok_or_else(|| {
                    wrong_type(
                        span,
                        name,
                        "a frequency in Hz (220) or a note (a3, c#4, eb2)",
                    )
                })?,
                _ => {
                    return Err(wrong_type(
                        span,
                        name,
                        "a frequency in Hz (220) or a note (a3, c#4, eb2)",
                    ));
                }
            };
            if !(min..=max).contains(&hz) {
                return Err(LangErr::new(
                    Code::OutOfRange,
                    span,
                    format!("{name} must be between {min} and {max} Hz, not {hz}"),
                ));
            }
            Ok(())
        }
        (SettingType::Rate, _) => Err(wrong_type(
            span,
            name,
            "periods (0.5), 4b, 250ms, 2s, 4800f, or a rate (5hz)",
        )),
        (SettingType::Number { min, max }, Value::Num(n))
        | (SettingType::Number { min, max }, Value::Db(n))
            if matches!(value, Value::Num(_)) || spec.db =>
        {
            let v = match value {
                Value::Db(_) => db_to_linear(eval_num(n)?),
                _ => eval_num(n)?,
            };
            if v < min || v > max {
                return Err(LangErr::new(
                    Code::OutOfRange,
                    span,
                    format!("{name} must be between {min} and {max}, not {v}"),
                ));
            }
            Ok(())
        }
        (SettingType::NumberOr { min, max, .. }, Value::Num(n)) => {
            let v = eval_num(n)?;
            if v < min || v > max {
                return Err(LangErr::new(
                    Code::OutOfRange,
                    span,
                    format!("{name} must be between {min} and {max}, not {v}"),
                ));
            }
            Ok(())
        }
        (SettingType::NumberOr { words, .. }, Value::Word(w))
            if words.contains(&w.name.as_str()) =>
        {
            Ok(())
        }
        (SettingType::NumberOr { words, .. }, _) => Err(wrong_type(
            span,
            name,
            &format!("a number, or {}", words.join(", ")),
        )),
        (SettingType::Int { min, max }, Value::Num(n)) => {
            let v = eval_num(n)?;
            if v.fract() != 0.0 {
                return Err(wrong_type(span, name, "a whole number"));
            }
            if v < min as f64 || v > max as f64 {
                return Err(LangErr::new(
                    Code::OutOfRange,
                    span,
                    format!("{name} must be between {min} and {max}, not {v}"),
                ));
            }
            Ok(())
        }
        // `fm=a+b`: only a setting that names waves (an open word list)
        (SettingType::Word(words), Value::Words(_)) if words.is_empty() && name == "fm" => Ok(()),
        (_, Value::Words(_)) => Err(wrong_type(span, name, "one value, not a+b")),
        (SettingType::Word(words), Value::Word(w)) => {
            if words.contains(&w.name.as_str()) {
                Ok(())
            } else {
                Err(LangErr::new(
                    Code::WrongType,
                    span,
                    format!("{name} takes one of: {}", words.join(", ")),
                ))
            }
        }
        (SettingType::Bool, Value::Word(w)) if w.name == "yes" || w.name == "no" => Ok(()),
        (SettingType::Str, Value::Str(_)) => Ok(()),
        (SettingType::Str, _) => Err(wrong_type(span, name, "a quoted string (\"name.wav\")")),
        (SettingType::DurationOr(words), Value::Word(w)) => {
            if words.contains(&w.name.as_str()) {
                Ok(())
            } else {
                let err = wrong_type(
                    span,
                    name,
                    &format!("a duration (0.5, 2b, 5ms, 480f) or {}", words.join(", ")),
                );
                // the old name of hold=retrigger
                Err(match (name, w.name.as_str()) {
                    ("hold", "hit") => err.with_hint("hold=retrigger"),
                    _ => err,
                })
            }
        }
        (SettingType::Duration | SettingType::DurationOr(_), Value::Num(n))
        | (SettingType::Duration | SettingType::DurationOr(_), Value::Duration(n, _)) => {
            let v = eval_num(n)?;
            if v < 0.0 {
                return Err(LangErr::new(
                    Code::OutOfRange,
                    span,
                    format!("{name} can't be negative"),
                ));
            }
            Ok(())
        }
        (SettingType::Clock, Value::Duration(..)) => Ok(()),
        (SettingType::Ticks, Value::Num(n)) | (SettingType::Ticks, Value::Duration(n, _)) => {
            let v = eval_num(n)?;
            if v < 0.0 {
                return Err(LangErr::new(
                    Code::OutOfRange,
                    span,
                    format!("{name} can't be negative"),
                ));
            }
            Ok(())
        }
        (SettingType::Number { .. } | SettingType::Int { .. }, _) => {
            Err(wrong_type(span, name, "a number"))
        }
        (SettingType::Word(words), _) => Err(wrong_type(
            span,
            name,
            &format!("one of: {}", words.join(", ")),
        )),
        (SettingType::Bool, _) => Err(wrong_type(span, name, "yes or no")),
        (SettingType::Duration, _) => {
            Err(wrong_type(span, name, "a duration (0.5, 2b, 5ms, 480f)"))
        }
        (SettingType::DurationOr(words), _) => Err(wrong_type(
            span,
            name,
            &format!("a duration (0.5, 2b, 5ms, 480f) or {}", words.join(", ")),
        )),
        (SettingType::Ticks, _) => Err(wrong_type(
            span,
            name,
            "beats or a duration (0.04, 2b, 10ms, 480f)",
        )),
        (SettingType::Clock, _) => Err(wrong_type(span, name, "a clock (120b, 500ms, 24000f)")),
    }
}

/// Checks the items of a `kind` statement for `name`: every relation and
/// setting must be one the kind takes, with the right type and range, and
/// what a new object needs must be there. Reports every problem it finds.
pub fn check_items<'a>(
    sc: &KindSchema,
    name: &str,
    items: &'a [Item],
    creating: bool,
    stmt: Span,
) -> Result<Checked<'a>, Vec<LangErr>> {
    let kind = kind_word(sc.kind);
    let mut errs = Vec::new();
    let mut out = Checked::default();

    for item in items {
        match item {
            Item::From(path, span) => {
                if sc.from == Req::No {
                    errs.push(wrong_type(*span, kind, "no 'from'"));
                } else if out.from_name.is_some() || out.from.replace((path, *span)).is_some() {
                    errs.push(wrong_type(*span, kind, "one 'from'"));
                }
            }
            Item::FromName(id) => {
                if sc.from == Req::No {
                    errs.push(wrong_type(id.span, kind, "no 'from'"));
                } else if out.from.is_some() || out.from_name.replace(id).is_some() {
                    errs.push(wrong_type(id.span, kind, "one 'from'"));
                }
            }
            Item::On(clock) => {
                let span = match clock {
                    ClockRef::Lit(c) => c.span,
                    ClockRef::Path(p) => p.first().map_or(stmt, |i| i.span),
                };
                if sc.kind == Kind::Lfo {
                    errs.push(LangErr::new(Code::WrongType, span, LFO_SPEED_HINT));
                } else if sc.on == Req::No {
                    errs.push(wrong_type(span, kind, "no 'on'"));
                } else if out.on.replace(clock).is_some() {
                    errs.push(wrong_type(span, kind, "one 'on'"));
                }
            }
            Item::In(n) => {
                if sc.kind == Kind::Lfo {
                    errs.push(LangErr::new(Code::WrongType, n.span(), LFO_SPEED_HINT));
                    continue;
                }
                if sc.period == Req::No {
                    errs.push(wrong_type(n.span(), kind, "no 'in'"));
                    continue;
                }
                if out.period.is_some() {
                    errs.push(wrong_type(n.span(), kind, "one 'in'"));
                    continue;
                }
                match eval_num(n) {
                    Ok(v) if v >= 1.0 && v.fract() == 0.0 && v <= f64::from(u32::MAX) => {
                        out.period = Some((v as u32, n.span()));
                    }
                    Ok(v) => errs.push(LangErr::new(
                        Code::OutOfRange,
                        n.span(),
                        format!("a period is a whole number of beats, at least 1, not {v}"),
                    )),
                    Err(e) => errs.push(e),
                }
            }
            Item::Setting(id, value, over) => {
                let Some(spec) = sc.settings.iter().find(|s| {
                    s.name == id.name || (id.name.len() == 1 && id.name.starts_with(s.short))
                }) else {
                    let e = LangErr::new(
                        Code::UnknownSetting,
                        id.span,
                        format!("{kind} has no setting '{}'", id.name),
                    )
                    .with_suggestion(&id.name, sc.settings.iter().map(|s| s.name));
                    errs.push(
                        if let Some(hint) = removed_setting_hint(sc.kind, &id.name) {
                            LangErr::new(
                                Code::UnknownSetting,
                                id.span,
                                format!("{kind} has no setting '{}'", id.name),
                            )
                            .with_hint(hint)
                        } else if e.suggestion.is_some() {
                            e
                        } else if sc.settings.is_empty() {
                            e.with_hint(format!("{kind} takes no settings"))
                        } else {
                            let names: Vec<&str> = sc.settings.iter().map(|s| s.name).collect();
                            e.with_hint(format!("{kind} takes: {}", names.join(", ")))
                        },
                    );
                    continue;
                };
                if out.setting(spec.name).is_some() {
                    errs.push(wrong_type(id.span, spec.name, "one value"));
                    continue;
                }
                if let Some(task) = spec.built {
                    errs.push(not_built(id.span, &format!("{kind} {}", spec.name), task));
                    continue;
                }
                if let Some(o) = over {
                    if !spec.glides {
                        errs.push(LangErr::new(
                            Code::WrongType,
                            o.span,
                            format!(
                                "{} changes at once; 'over' needs a setting that can glide",
                                spec.name
                            ),
                        ));
                        continue;
                    }
                    if let Err(e) = check_over(o) {
                        errs.push(e);
                        continue;
                    }
                }
                match check_value(spec, value, id.span) {
                    Ok(()) => out.settings.push(CheckedSetting {
                        spec,
                        value,
                        over: over.as_ref(),
                        span: id.span,
                    }),
                    Err(e) => errs.push(e),
                }
            }
            Item::Step(s) => {
                if sc.positional == Positional::Steps {
                    out.steps.push(s);
                } else {
                    errs.push(wrong_type(s.span, kind, "no steps"));
                }
            }
            Item::Clock(c) => {
                if sc.positional != Positional::Clock {
                    errs.push(wrong_type(c.span, kind, "no clock rate"));
                } else if out.clock.replace(c).is_some() {
                    errs.push(wrong_type(c.span, kind, "one clock rate"));
                } else if let Some(o) = &c.over
                    && let Err(e) = check_over(o)
                {
                    errs.push(e);
                }
            }
        }
    }

    if sc.kind == Kind::Seq
        && errs.is_empty()
        && let Some(gen_steps) = out.setting("steps")
        && !out.steps.is_empty()
    {
        errs.push(LangErr::new(
            Code::WrongType,
            gen_steps.span,
            "seq takes steps or steps=, not both",
        ));
    }

    if creating && errs.is_empty() {
        let mut missing = Vec::new();
        if sc.from == Req::RequiredOnCreate && out.from.is_none() && out.from_name.is_none() {
            missing.push(if sc.kind == Kind::Track {
                "from \"file\""
            } else {
                "from \"track\" (or from BUFFER)"
            });
        }
        if sc.on == Req::RequiredOnCreate && out.on.is_none() {
            missing.push("on <clock>");
        }
        if sc.period == Req::RequiredOnCreate && out.period.is_none() {
            missing.push("in <beats>");
        }
        if sc.positional == Positional::Steps
            && out.steps.is_empty()
            && !(sc.kind == Kind::Seq && out.setting("steps").is_some())
        {
            missing.push("steps (1 2 3 4)");
        }
        if sc.kind == Kind::Lfo {
            if out.setting("to").is_none() {
                missing.push("to=<target>");
            }
            if out.setting("depth").is_none() {
                missing.push("depth=<amount>");
            }
        }
        if sc.positional == Positional::Clock && out.clock.is_none() {
            missing.push("a clock rate (120b, 500ms, 24000f)");
        }
        if !missing.is_empty() {
            errs.push(LangErr::new(
                Code::MissingItem,
                stmt,
                format!("{kind} '{name}' needs {}", missing.join(" and ")),
            ));
        }
    }

    if errs.is_empty() { Ok(out) } else { Err(errs) }
}

#[cfg(test)]
mod tests {
    use super::super::help::kinds;
    use super::*;

    #[test]
    fn short_forms_are_unique_per_kind() {
        for kind in kinds() {
            let sc = schema(kind);
            for (i, a) in sc.settings.iter().enumerate() {
                // a name of one letter is its own short form (`q`)
                assert!(
                    a.name.len() > 1 || a.name == a.short.to_string(),
                    "{} is not a long name",
                    a.name
                );
                assert!(
                    !sc.settings
                        .iter()
                        .any(|b| b.name != a.name && b.name == a.short.to_string()),
                    "{kind:?}: short '{}' equals a long name",
                    a.short
                );
                for b in &sc.settings[i + 1..] {
                    assert_ne!(a.short, b.short, "{kind:?}: {} and {}", a.name, b.name);
                }
            }
        }
    }

    #[test]
    fn every_kind_has_a_schema() {
        for kind in kinds() {
            assert_eq!(schema(kind).kind, kind);
        }
    }

    #[test]
    fn every_action_is_listed() {
        for word in [
            ActionWord::Start,
            ActionWord::Stop,
            ActionWord::Pause,
            ActionWord::Resume,
            ActionWord::Remove,
            ActionWord::Ungroup,
            ActionWord::Rescan,
        ] {
            assert_eq!(action_spec(word).word, action_word(word));
        }
    }
}
