//! `help` (REQ-056 §7), generated from the schemas: the list of kinds and
//! actions, and one schema in full. The card (`card.rs`) draws on the same
//! tables.

use super::ast::{Ident, Kind};
use super::errors::{Code, LangErr};
use super::schema::{
    ACTIONS, KindSchema, Positional, Req, SettingSpec, SettingType, kind_word, schema,
};
use crate::audio_processing::engine::FxType;

/// Every kind, in the order the docs list them: the effects come from the
/// registry, between `env` and `wave`.
pub fn kinds() -> impl Iterator<Item = Kind> {
    [
        Kind::Clock,
        Kind::Track,
        Kind::Voice,
        Kind::Group,
        Kind::Seq,
        Kind::Packet,
        Kind::Lfo,
        Kind::Env,
    ]
    .into_iter()
    .chain(FxType::ALL.iter().map(|t| Kind::Fx(*t)))
    .chain([Kind::Wave, Kind::Buffer])
}

/// Words that aren't kinds or actions: the script words and the queries.
pub const OTHER_WORDS: &[(&str, &str)] = &[
    (
        "let",
        "let name = value: a number, clock or duration; use as $name or (expr)",
    ),
    (
        "source",
        "source NAME [read] [history] [stop]: runs NAME (sources/NAME.blast; or a quoted path); read prints it without running; history adds its statements to the history (Up) without running them; stop cancels a running script of that name (it stops nothing)",
    ),
    (
        "state",
        "state [path] [to NAME]: the session (or one object) as a script; to writes it to sources/NAME.blast",
    ),
    (
        "and",
        "a and b: both happen in the same instant (binds tighter than then)",
    ),
    (
        "then",
        "a then b: b runs once a is done (its longest glide is over), whatever happened to it",
    ),
    (
        "thenif",
        "a thenif b: like then, but only if no glide of a was cut short; otherwise the rest of the line is dropped (W105)",
    ),
    (
        "wait",
        "wait D [by clock] then b: b runs after D (beats of main unless by clock; 500ms is clock time). On a line of its own in a script, the lines after it run D later and the script runs in the background",
    ),
    (
        "end",
        "end: in a script, stops what the script started and cancels its remaining lines; end after D waits D first; end NAME (at the prompt) ends a running script",
    ),
    ("check", "check NAME: checks a script file, changes nothing"),
    ("help", "help [kind|action]: this text"),
    ("quit", "quit: leaves blast (exit does the same)"),
    ("exit", "exit: same as quit"),
    ("clear", "clear: clears the window"),
];

/// The type and range of a setting, in words.
pub fn type_text(ty: SettingType) -> String {
    match ty {
        SettingType::Number { min, max } => format!("number {min}..{max}"),
        SettingType::Int { min, max } => format!("whole number {min}..{max}"),
        SettingType::Word([]) => "a word".to_string(),
        SettingType::Word(words) => words.join("|"),
        SettingType::Duration => "duration (0.5, 2b, 5ms, 480f)".to_string(),
        SettingType::DurationOr(words) => {
            format!("duration (0.5, 2b, 5ms, 480f) or {}", words.join("|"))
        }
        SettingType::Clock => "clock (120b, 500ms, 24000f)".to_string(),
        SettingType::Ticks => "beats or a duration (0.04, 2b, 10ms, 480f)".to_string(),
        SettingType::Rate => "periods (0.5), 4b, 250ms, 2s, 4800f, or a rate (5hz)".to_string(),
        SettingType::Freq { min, max } => {
            format!("Hz {min}..{max}, or a note (a3, c#4, eb2)")
        }
        SettingType::NumberOr { min, max, words } => {
            format!("number {min}..{max}, or {}", words.join("|"))
        }
        SettingType::Bool => "yes|no".to_string(),
        SettingType::Str => "a quoted string".to_string(),
    }
}

/// `name=value` settings of a kind that work today.
pub fn built_settings(sc: &KindSchema) -> impl Iterator<Item = &'static SettingSpec> {
    sc.settings.iter().filter(|s| s.built.is_none())
}

/// The relations a kind takes, as written: `from`, `on`, `in`.
pub fn relations(sc: &KindSchema) -> Vec<&'static str> {
    let mut out = Vec::new();
    if sc.from != Req::No {
        out.push("from");
    }
    if sc.on != Req::No {
        out.push("on");
    }
    if sc.period != Req::No {
        out.push("in");
    }
    out
}

fn kind_by_word(word: &str) -> Option<Kind> {
    kinds().find(|k| kind_word(*k) == word)
}

fn req_text(r: Req) -> &'static str {
    match r {
        Req::No => "no",
        Req::Optional => "optional",
        Req::RequiredOnCreate => "required to create",
    }
}

fn kind_line(kind: Kind) -> String {
    let sc = schema(kind);
    let mut line = format!("  {:<8} {}", kind_word(kind), sc.doc);
    if let Some(task) = sc.built {
        line.push_str(&format!(" (not built yet: {task})"));
    }
    line
}

fn kind_page(kind: Kind) -> String {
    let sc = schema(kind);
    let mut out = format!("{}: {}\n", kind_word(kind), sc.doc);
    if let Some(task) = sc.built {
        out.push_str(&format!("  not built yet ({task})\n"));
    }
    for (req, what) in [
        (
            sc.from,
            if kind == Kind::Track {
                "from \"file\""
            } else {
                "from \"track\""
            },
        ),
        (sc.on, "on <clock|group|voice|120b>"),
        (sc.period, "in <beats> (the period)"),
    ] {
        if req != Req::No {
            out.push_str(&format!("  {what}: {}\n", req_text(req)));
        }
    }
    match sc.positional {
        Positional::No => {}
        Positional::Clock => out.push_str(
            "  takes a clock rate: 120b, 500ms or 24000f\n  \
             the rate can glide unless the clock is stopped or new (E409): clock main 140b over 8 (a bare number is periods of 4\n  \
             beats, as a clock has no 'in'; 8b beats; 2s time; linear in bpm for 'b')\n",
        ),
        Positional::Steps => {
            out.push_str("  takes steps: 1 2 3.5 4%50 1..4~0.02 (counting from 1)\n")
        }
    }
    out.push_str(match sc.block {
        super::schema::BlockUse::No => "  block: none\n",
        super::schema::BlockUse::Members => "  block: members, voices and processes\n",
        super::schema::BlockUse::Processes => "  block: processes it owns\n",
    });
    if sc.pipe {
        out.push_str(
            "  pipe: | receiver, ...  (trig [voice|group]: restart; melo [voice] 36 43: next note, 60 = no shift)\n",
        );
    }
    if sc.settings.is_empty() {
        out.push_str("  settings: none\n");
    } else {
        out.push_str("  settings:\n");
        for s in sc.settings {
            out.push_str(&format!(
                "    {:<14} {}; default {}; {}",
                format!("{} ({})", s.name, s.short),
                type_text(s.ty),
                if s.default.is_empty() { "-" } else { s.default },
                s.doc
            ));
            if s.glides {
                out.push_str(", glides");
            }
            if let Some(task) = s.built {
                out.push_str(&format!(" (not built yet: {task})"));
            }
            out.push('\n');
        }
        if sc.settings.iter().any(|s| s.glides) {
            out.push_str(
                "  'glides' settings take 'over D' after the value: snare r=2 over 1 (a bare\n  number is periods; 2b beats; 250ms time)\n",
            );
        }
    }
    if kind == Kind::Group {
        out.push_str(
            "  editing:\n    \
             group drums { hat }      adds a top-level voice; a voice in another group moves\n    \
             ungroup drums.hat        takes one member out, on the clock it hears\n    \
             ungroup drums            dissolves the group; the members stay\n    \
             remove drums             deletes the group with its members\n    \
             remove all               deletes everything except main\n    \
             drums gain=0 over 2      fades the members; drums in 8 sets the group's period\n",
        );
    }
    out.trim_end().to_string()
}

/// `help`, or `help <kind|action|word>`.
pub fn help(topic: Option<&Ident>) -> Result<String, LangErr> {
    let Some(topic) = topic else {
        let mut out = String::from("Kinds (help <kind>):\n");
        for k in kinds() {
            out.push_str(&kind_line(k));
            out.push('\n');
        }
        out.push_str("Actions (help <action>):\n");
        for a in ACTIONS {
            out.push_str(&format!("  {:<8} {}", a.word, a.doc));
            if let Some(task) = a.built {
                out.push_str(&format!(" (not built yet: {task})"));
            }
            out.push('\n');
        }
        out.push_str("Script and queries:\n");
        for (word, doc) in OTHER_WORDS {
            out.push_str(&format!("  {word:<8} {doc}\n"));
        }
        return Ok(out.trim_end().to_string());
    };
    let word = topic.name.as_str();
    if let Some(kind) = kind_by_word(word) {
        return Ok(kind_page(kind));
    }
    if let Some(a) = ACTIONS.iter().find(|a| a.word == word) {
        let mut out = format!("{}: {}\n  takes: {}", a.word, a.doc, a.takes);
        if let Some(task) = a.built {
            out.push_str(&format!("\n  not built yet ({task})"));
        }
        return Ok(out);
    }
    if let Some((w, doc)) = OTHER_WORDS.iter().find(|(w, _)| *w == word) {
        return Ok(format!("{w}: {doc}"));
    }

    // a setting name points at the kinds that have it
    let owners: Vec<&str> = kinds()
        .filter(|k| schema(*k).settings.iter().any(|s| s.name == word))
        .map(kind_word)
        .collect();
    let err = LangErr::new(
        Code::UnknownName,
        topic.span,
        format!("no help for '{word}'"),
    );
    if let Some(first) = owners.first() {
        return Err(err.with_hint(format!(
            "'{word}' is a setting of {}; try 'help {first}'",
            owners.join(" and ")
        )));
    }
    let all = kinds()
        .map(kind_word)
        .chain(ACTIONS.iter().map(|a| a.word))
        .chain(OTHER_WORDS.iter().map(|(w, _)| *w));
    Err(err.with_suggestion(word, all))
}
