use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::audio_processing::{
    blast_rand::{X128P, fast_seed},
    blast_time::blast_time::{TempoMode, TempoUnit},
};
use crate::file_parsing::decode_helpers::AudioFile;

pub struct CmdQueue {
    buf: Vec<UnsafeCell<Option<Command>>>,
    cap: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
}

unsafe impl Send for CmdQueue {}
unsafe impl Sync for CmdQueue {}

impl CmdQueue {
    pub fn new(cap: usize) -> Self {
        let mut buf = Vec::<UnsafeCell<Option<Command>>>::with_capacity(cap);

        for _ in 0..cap {
            buf.push(UnsafeCell::new(None));
        }

        Self {
            buf,
            cap,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    pub fn try_push(&self, cmd: Command) -> Result<(), String> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);

        if (head + 1) % self.cap == tail {
            return Err(String::from("Command queue full"));
        }

        unsafe {
            *self.buf[head].get() = Some(cmd);
        }

        self.head.store((head + 1) % self.cap, Ordering::Release);
        Ok(())
    }

    // only meaningful on the producer side: the consumer can only
    // make room, so a queue that isn't full stays that way until
    // the producer pushes
    pub fn is_full(&self) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);

        (head + 1) % self.cap == tail
    }

    pub fn try_pop(&self) -> Option<Command> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);

        if head == tail {
            return None;
        }

        let cmd = unsafe { (*self.buf[tail].get()).take() };

        self.tail.store((tail + 1) % self.cap, Ordering::Release);

        cmd
    }
}

use blast_macros::var_args;

macro_rules! commands {
    ( $( $var:ident ),* $(,)? ) => {
        pub enum Command {
            $(
                $var(var_args!($var)), // formats as {CmdType}Args
            )*
        }

        unsafe impl Send for Command {}
        unsafe impl Sync for Command {}
    }
}

commands! {
    // Voices
    Load,
    Start,
    Pause,
    Resume,
    Stop,
    Unload,
    Velocity,
    // Groups
    Group,
    Tc,
    // Processes
    Seq,
    // Program
    Quit,
}

// specialized args for commands
// (need definition because they're declared in the commands! macro)

pub struct LoadArgs {
    pub track_idx: usize,
    pub tempo_repr: TempoRepr,
}

pub struct StartArgs {
    pub idx: Idx,
}

pub struct PauseArgs {
    pub idx: Idx,
}

pub struct ResumeArgs {
    pub idx: Idx,
}

pub struct StopArgs {
    pub idx: Idx,
}

pub struct UnloadArgs {
    pub idx: Idx, // Voice or GroupVoice
}

pub struct VelocityArgs {
    pub idx: Idx,
    pub val: f32,
}

pub struct GroupArgs {
    pub tempo: TempoRepr,
    pub vs_fs_ps: Vec<(usize, bool, Vec<usize>)>,
    // in Group order (entry i becomes the Group's Voice i,
    // matching the VoiceRepr idx in the GroupRepr),
    // store the ids Voice
    // with whether or not its TempoState refers to the Group's
    // and with the ids of all of the Processes
    // whose TempoStates refer to the Group's
}

pub struct TcArgs {
    pub tempo: TempoRepr,
}

pub struct SeqArgs {
    pub idx: Idx,
    pub tempo: TempoRepr,
    pub period: usize,
    pub steps: Vec<f32>,
    pub chance: Vec<f32>,
    pub jit: Vec<f32>,
    pub rng: X128P,
}

// doesn't need any members, just triggers raise(SIGTERM)
pub struct QuitArgs {}

// structs to represent engine/object state

// use for terse, ambiguous Commands like Start;
// prefer Reprs when more info is required
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Idx {
    Tempo(usize),
    Voice(usize),
    Process(usize),
    Group(usize),
    // a Voice inside a Group: (Group idx, Voice idx in that Group);
    // Voice is only for Voices in Conductor.voices
    GroupVoice(usize, usize),
    // don't need one for Track because TrackRepr is already
    // just an index, and there are few Commands that operate on
    // Tracks, so it'll never be ambiguous
}

pub struct TrackRepr {
    idx: usize,
}

impl TrackRepr {
    fn new(idx: usize) -> Self {
        Self { idx }
    }
}

// owned bool determines whether a TempoState is initialized
// or cloned inside of the engine
pub struct TempoRepr {
    pub idx: usize,
    pub owned: bool,
    pub mode: TempoMode,
    pub unit: TempoUnit,
    pub interval: f32,
}

impl TempoRepr {
    fn new(idx: usize) -> Self {
        Self {
            idx,
            owned: true, // default owned, until clone_owner
            mode: TempoMode::TBD,
            unit: TempoUnit::Samples,
            interval: 0f32,
        }
    }

    pub fn clone(other: &TempoRepr) -> Self {
        Self {
            idx: other.idx,
            owned: other.owned,
            mode: other.mode,
            unit: other.unit,
            interval: other.interval,
        }
    }

    // this is used when referring to another object's TempoState
    fn clone_owner(other: &TempoRepr) -> Self {
        Self {
            idx: other.idx,
            owned: false,
            mode: other.mode,
            unit: other.unit,
            interval: other.interval,
        }
    }

    // like clone_owner, but for referring to the TempoState of a Voice
    // or Group: if that object owns its TempoState, point at the
    // object's current idx in the engine (its TempoRepr's own idx
    // is meaningless); otherwise it already refers to something else
    // (e.g. a TempoContext), so keep that reference
    fn clone_from(other: &TempoRepr, owner_idx: usize) -> Self {
        let mut repr = TempoRepr::clone_owner(other);
        if other.owned {
            repr.idx = owner_idx;
        }
        repr
    }

    fn init(&mut self, mode: TempoMode, unit: TempoUnit, interval: f32) {
        self.mode = mode;
        self.unit = unit;
        self.interval = interval;
    }
}

pub struct VoiceRepr {
    idx: usize,
    tempo: TempoRepr,
    processes: HashMap<String, ProcRepr>,
    proc_tempi: HashMap<usize, TempoRepr>,
}

impl VoiceRepr {
    fn new(idx: usize, tempo: TempoRepr) -> Self {
        Self {
            idx,
            tempo,
            processes: HashMap::<String, ProcRepr>::new(),
            proc_tempi: HashMap::<usize, TempoRepr>::new(),
        }
    }
}

pub struct ProcRepr {
    // Processes are difficult to represent because they all
    // differ, so can only represent info that applies
    // to all Processes
    //
    idx: usize, // index of the Process in its owner's
    // Vec<Process>
    #[allow(dead_code)] // not read yet; kept for targeting Processes
    owner_idx: Idx, // index of the Process's $owner
    // in the engine's Vec<$owner>
    tempo: Option<TempoRepr>,
    // maybe create ProcArgs enum, one for each Process
}

impl ProcRepr {
    fn new(idx: usize, owner_idx: Idx, tempo: Option<TempoRepr>) -> Self {
        Self {
            idx,
            owner_idx,
            tempo,
        }
    }
}

pub struct GroupRepr {
    idx: usize,
    tempo: TempoRepr,
    voices: HashMap<String, VoiceRepr>,
}

impl GroupRepr {
    fn new(idx: usize, tempo: TempoRepr, voices: HashMap<String, VoiceRepr>) -> Self {
        Self { idx, tempo, voices }
    }
}

// keeps track of all entities' states
pub struct EngineState {
    tracks: HashMap<String, TrackRepr>,
    voices: HashMap<String, VoiceRepr>,
    groups: HashMap<String, GroupRepr>,
    tempo_cons: HashMap<String, TempoRepr>,
    #[allow(dead_code)] // not read yet; for validating routing
    out_channels: usize,
}

impl EngineState {
    pub fn new(files: Vec<AudioFile>, out_channels: usize) -> Self {
        let mut tracks: HashMap<String, TrackRepr> = HashMap::new();
        for (idx, af) in files.iter().enumerate() {
            tracks.insert(af.file_name.clone(), TrackRepr::new(idx));
        }

        Self {
            tracks,
            out_channels,
            voices: HashMap::<String, VoiceRepr>::new(),
            groups: HashMap::<String, GroupRepr>::new(),
            tempo_cons: HashMap::<String, TempoRepr>::new(),
        }
    }
}

// validates and formats Commands for the engine
// (handles string allocations, integer/float parsing, etc)
pub struct CmdProcessor {
    pub engine_state: EngineState,
}

impl CmdProcessor {
    pub fn new(engine_state: EngineState) -> Self {
        Self { engine_state }
    }

    pub fn parse(&mut self, cmd: String) -> CmdResult<Command> {
        let mut parts = cmd.splitn(2, ' ');
        let cmd = parts.next().unwrap();
        let args = parts.next().unwrap_or_else(|| "").to_string();

        match cmd {
            "load" => self.try_load(args),
            "start" => self.try_start(args),
            "pause" => self.try_pause(args),
            "resume" => self.try_resume(args),
            "stop" => self.try_stop(args),
            "unload" => self.try_unload(args),
            "velocity" => self.try_velocity(args),
            "group" => self.try_group(args),
            "tc" | "tempocon" => self.try_tc(args),
            "seq" => self.try_seq(args),
            "q" | "quit" => Ok(Command::Quit(QuitArgs {})),
            _ => {
                return Err(CmdErr::NoCmd {
                    cmd: cmd.to_owned(),
                });
            }
        }
    }

    // parse cmd and push it to the engine;
    // parse() updates the mirror, so make sure the push can't fail
    // afterwards (the command thread is the only producer, so a
    // queue with room before parse still has room after)
    pub fn submit(&mut self, cmd: String, queue: &CmdQueue) -> Result<(), String> {
        if queue.is_full() {
            return Err(String::from("Command queue full"));
        }

        let valid = self.parse(cmd).map_err(|error| error.to_string())?;
        queue.try_push(valid)
    }

    // CmdResults (returned directly to command thread)
    //
    fn try_load(&mut self, args: String) -> CmdResult<Command> {
        // parse args to:
        // - validate that the Track exists
        // - get the Track's idx
        // - format TempoRepr
        // - format VoiceRepr for engine
        //
        // engine then parses LoadArgs to:
        // - get the Track
        // - create a TempoState based on the TempoRepr
        //      - this involves checking TempoRepr.mode
        //        to see if the Voice's TempoState refers to
        //        an existing TempoState
        // - call Voice::new(track, tempo_state)
        //
        let mut args = args.split_whitespace();
        let name = args.next().ok_or(CmdErr::MissingArg {
            arg: "name".to_string(),
            cmd: "load".to_string(),
        })?;
        let name = name.to_string();

        let track = self.find_track(name.clone())?;
        let track_idx = track.idx;

        // initialize tempo_repr with an idx of 0 because
        // a Voice will only ever have one personal TempoState
        let mut tempo_repr = TempoRepr::new(0usize);

        // if a Voice by this name (currently the track name)
        // already exists, then return error
        match self.find_voice(name.clone()) {
            Ok(_) => {
                return Err(CmdErr::AlreadyIs {
                    ty: "Voice".to_string(),
                    name: name,
                });
            }
            Err(_) => (),
        }

        while let Some(arg) = args.next() {
            match arg {
                "-t" | "--tempo" => {
                    let t_arg = args.next().ok_or(CmdErr::MissingArg {
                        arg: "unit".to_string(),
                        cmd: "load -t/--tempo".to_string(),
                    })?;

                    let mut t_args = t_arg.split(':');

                    let u = t_args.next().unwrap();
                    if u == "c" {
                        // find TempoContext
                        let tc_name = t_args.next().ok_or(CmdErr::MissingArg {
                            arg: "name".to_string(),
                            cmd: "load -t c:???".to_string(),
                        })?;

                        tempo_repr = match self.find_tc(tc_name.to_string()) {
                            Ok(tc) => TempoRepr::clone_owner(&tc),
                            Err(error) => return Err(error.into()),
                        };
                        continue;
                    }
                    if u == "g" {
                        // find Group
                        let g_name = t_args.next().ok_or(CmdErr::MissingArg {
                            arg: "name".to_string(),
                            cmd: "load -t g:???".to_string(),
                        })?;

                        tempo_repr = match self.find_group(g_name.to_string()) {
                            Ok(g) => TempoRepr::clone_from(&g.tempo, g.idx),
                            Err(error) => return Err(error.into()),
                        };
                        continue;
                    }

                    // make new TempoState from matched arguments

                    let unit = match u {
                        "s" => TempoUnit::Samples,
                        "m" => TempoUnit::Millis,
                        "b" => TempoUnit::Bpm,
                        _ => {
                            return Err(CmdErr::InvalidArg {
                                arg: u.to_owned(),
                                cmd: "load -t".to_string(),
                            });
                        }
                    };

                    let interval = t_args
                        .next()
                        .ok_or(CmdErr::MissingArg {
                            arg: "interval".to_string(),
                            cmd: "load -t".to_string(),
                        })
                        .and_then(|raw| {
                            raw.parse::<f32>().map_err(|_| CmdErr::InvalidArg {
                                arg: raw.to_owned(),
                                cmd: "load -t".to_string(),
                            })
                        })?;

                    tempo_repr.init(TempoMode::Voice, unit, interval);
                }
                // no argument matched
                _ => {
                    return Err(CmdErr::InvalidArg {
                        arg: arg.to_owned(),
                        cmd: "load".to_string(),
                    });
                }
            }
        }
        // if this is the first Voice,
        // it will be indexed at 0
        let idx = self.engine_state.voices.len();
        self.engine_state
            .voices
            .insert(name, VoiceRepr::new(idx, TempoRepr::clone(&tempo_repr)));

        Ok(Command::Load(LoadArgs {
            track_idx,
            tempo_repr,
        }))
    }

    // the following could start multiple things at the same time
    // (e.g. *Args could hold a Vec<Idx>);
    // maybe implement "all" as a reserved word
    //
    fn try_start(&mut self, args: String) -> CmdResult<Command> {
        let (ty, name) = self.parse_type_and_name(args, "start".to_string())?;
        let idx = self.get_idx(ty, name)?;
        Ok(Command::Start(StartArgs { idx }))
    }

    fn try_pause(&mut self, args: String) -> CmdResult<Command> {
        let (ty, name) = self.parse_type_and_name(args, "pause".to_string())?;
        let idx = self.get_idx(ty, name)?;
        Ok(Command::Pause(PauseArgs { idx }))
    }

    fn try_resume(&mut self, args: String) -> CmdResult<Command> {
        let (ty, name) = self.parse_type_and_name(args, "resume".to_string())?;
        let idx = self.get_idx(ty, name)?;
        Ok(Command::Resume(ResumeArgs { idx }))
    }

    fn try_stop(&mut self, args: String) -> CmdResult<Command> {
        let (ty, name) = self.parse_type_and_name(args, "stop".to_string())?;
        let idx = self.get_idx(ty, name)?;
        Ok(Command::Stop(StopArgs { idx }))
    }

    fn try_unload(&mut self, name: String) -> CmdResult<Command> {
        // accepts voice or group.voice; gets idx and removes
        // the VoiceRepr from its owner (engine_state or a Group)
        let name = name.trim().to_string();
        let idx = self.find_voice_idx(name.clone())?;

        let (voices, v_name) = match name.split_once('.') {
            Some((group, voice)) => (&mut self.find_group(group.to_string())?.voices, voice),
            None => (&mut self.engine_state.voices, name.as_str()),
        };
        let removed = voices.remove(v_name).unwrap().idx; // found above

        // since all Voices after the removed Voice will be
        // shifted to the left, decrease all VoiceReprs with
        // an idx greater than the removed Voice's
        for (_, voice) in voices.iter_mut() {
            if voice.idx > removed {
                voice.idx -= 1;
            }
        }

        Ok(Command::Unload(UnloadArgs { idx }))
    }

    fn try_velocity(&mut self, args: String) -> CmdResult<Command> {
        let mut args = args.splitn(2, ' ');

        let name = args.next().ok_or(CmdErr::MissingArg {
            arg: "name".to_string(),
            cmd: "velocity".to_string(),
        })?;

        // Idx::Voice or Idx::GroupVoice
        let idx = self.get_idx("-v".to_string(), name.to_string())?;

        let val = args
            .next()
            .ok_or(CmdErr::MissingArg {
                arg: "value".to_string(),
                cmd: "velocity".to_string(),
            })
            .and_then(|raw| {
                raw.parse::<f32>().map_err(|_| CmdErr::InvalidArg {
                    arg: raw.to_owned(),
                    cmd: "velocity".to_string(),
                })
            })?;

        Ok(Command::Velocity(VelocityArgs { idx, val }))
    }

    fn try_group(&mut self, args: String) -> CmdResult<Command> {
        let mut args = args.split_whitespace();
        let name = args.next().ok_or(CmdErr::MissingArg {
            arg: "name".to_string(),
            cmd: "group".to_string(),
        })?;

        // a second Group by the same name would replace the first
        // in the mirror but not in the engine, shifting every later idx
        if self.engine_state.groups.contains_key(name) {
            return Err(CmdErr::AlreadyIs {
                ty: "Group".to_string(),
                name: name.to_string(),
            });
        }

        // -t tempo -v voices

        let mut tempo = TempoRepr::new(0);
        tempo.init(TempoMode::Group, TempoUnit::Bpm, 240.0);
        // names of the Voices to group, in command-line (Group) order;
        // nothing in engine_state changes until all args are valid,
        // so a failed command leaves the mirror untouched
        let mut v_names = Vec::<String>::new();

        while let Some(arg) = args.next() {
            match arg {
                "-t" | "--tempo" => match args.next() {
                    Some(t) => {
                        let mut t_args = t.split(':');
                        let u_str = t_args.next().ok_or(CmdErr::MissingArg {
                            arg: "unit".to_string(),
                            cmd: "group -t".to_string(),
                        })?;

                        if u_str == "c" {
                            let tc_name = t_args.next().ok_or(CmdErr::MissingArg {
                                arg: "TempoContext name".to_string(),
                                cmd: "group -t".to_string(),
                            })?;

                            match self.find_tc(tc_name.to_string()) {
                                Ok(tc) => tempo = TempoRepr::clone_owner(&tc),
                                Err(error) => return Err(error.into()),
                            }
                        } else {
                            let unit = match u_str {
                                "s" => TempoUnit::Samples,
                                "m" => TempoUnit::Millis,
                                "b" => TempoUnit::Bpm,
                                _ => {
                                    return Err(CmdErr::InvalidArg {
                                        arg: u_str.to_string(),
                                        cmd: "group -t".to_string(),
                                    });
                                }
                            };
                            let interval = t_args
                                .next()
                                .ok_or(CmdErr::MissingArg {
                                    arg: "interval".to_string(),
                                    cmd: "group -t".to_string(),
                                })
                                .and_then(|raw| {
                                    raw.parse::<f32>().map_err(|_| CmdErr::InvalidArg {
                                        arg: raw.to_string(),
                                        cmd: "group -t".to_string(),
                                    })
                                })?;

                            let mut new_tempo = TempoRepr::new(0);
                            new_tempo.init(TempoMode::Group, unit, interval);
                            tempo = new_tempo;
                        }
                    }
                    None => {
                        return Err(CmdErr::MissingArg {
                            arg: "arguments".to_string(),
                            cmd: "group -t".to_string(),
                        });
                    }
                },
                "-v" | "--voices" => match args.next() {
                    Some(v) => {
                        for name in v.split(',') {
                            let name = name.to_string();
                            if !self.engine_state.voices.contains_key(&name) {
                                return Err(CmdErr::NoVoice {
                                    name: name,
                                    group: None,
                                });
                            }
                            if v_names.contains(&name) {
                                return Err(CmdErr::Formatting {
                                    err: format!("Voice '{}' listed more than once", name),
                                });
                            }
                            v_names.push(name);
                        }
                    }
                    None => {
                        return Err(CmdErr::MissingArg {
                            arg: "arguments".to_string(),
                            cmd: "group -v".to_string(),
                        });
                    }
                },
                _ => {
                    return Err(CmdErr::InvalidArg {
                        arg: arg.to_owned(),
                        cmd: "group".to_string(),
                    });
                }
            }
        }

        // all args are valid; move the Voices out of the top level
        //
        // kept in command-line (Group) order, so that flags and
        // Process ids below line up with v_ids
        let mut voices = Vec::<(String, VoiceRepr)>::new();
        // save Voice indices as Voices are collected,
        // since these indices will change when added to voices
        let mut v_ids = Vec::<usize>::new();

        for name in v_names {
            let mut voice = self.engine_state.voices.remove(&name).unwrap();
            v_ids.push(voice.idx);
            voice.idx = voices.len(); // assign new index
            // in Group's Vec
            voices.push((name, voice));
        }

        // sort removed voices in reverse
        // so that the remaining voice.idx
        // are decremented correctly
        let mut sorted = v_ids.clone();
        sorted.sort_by(|a, b| b.cmp(a));

        for removed_id in sorted {
            for (_, v) in &mut self.engine_state.voices {
                if v.idx > removed_id {
                    v.idx -= 1;
                }
            }
        }

        // assign flags to each Voice depending on whether its
        // TempoState will refer to the Group's
        let mut v_flags: Vec<bool> = Vec::new();

        // collect indices of Processes whose TempoStates are being
        // assigned to the Group's TempoState
        let mut p_ids: Vec<Vec<usize>> = Vec::new();

        let g_idx = self.engine_state.groups.len();

        for (_, voice) in &mut voices {
            // if the Voice wasn't assigned a TempoState at birth,
            // it takes on the TempoState of the Group
            // (this is how a Voice's Process is synced with a Group's TempoState
            // [by proxy, if the Process refers to its Voice's TempoState])
            let mut p_i: Vec<usize> = Vec::new();

            if voice.tempo.mode == TempoMode::TBD {
                voice.tempo = TempoRepr::clone_from(&tempo, g_idx);
                v_flags.push(true);
                for (_, process) in &mut voice.processes {
                    // checks if any Process tempo has TempoMode::TBD
                    // (i.e. it was assigned to its Voice's
                    // uninitialized tempo, in anticipation of the
                    // Voice being added to a Group later)
                    match &process.tempo {
                        Some(t) => {
                            // only re-point Processes that follow
                            // the Voice; ones with their own (or a
                            // shared) TempoState keep it
                            if t.mode == TempoMode::TBD {
                                process.tempo = Some(TempoRepr::clone_from(&tempo, g_idx));
                                p_i.push(process.idx);
                            }
                        }
                        None => (),
                    }
                }
            } else {
                v_flags.push(false);
            }

            p_ids.push(p_i);
        }

        let group = GroupRepr::new(
            g_idx,
            TempoRepr::clone(&tempo),
            voices.into_iter().collect(),
        );

        self.engine_state.groups.insert(name.to_string(), group);

        // keep Group order; the engine removes Voices from
        // Conductor.voices in reverse itself
        let vs_fs_ps: Vec<(usize, bool, Vec<usize>)> = v_ids
            .into_iter()
            .zip(v_flags)
            .zip(p_ids)
            .map(|((a, b), c)| (a, b, c))
            .collect();

        Ok(Command::Group(GroupArgs { tempo, vs_fs_ps }))
    }

    fn try_tc(&mut self, args: String) -> CmdResult<Command> {
        let mut args = args.split_whitespace();
        let name = args.next().ok_or(CmdErr::MissingArg {
            arg: "name".to_string(),
            cmd: "tempocon".to_string(),
        })?;

        // see try_group
        if self.engine_state.tempo_cons.contains_key(name) {
            return Err(CmdErr::AlreadyIs {
                ty: "TempoContext".to_string(),
                name: name.to_string(),
            });
        }

        let tempo = args.next().ok_or(CmdErr::MissingArg {
            arg: "tempo".to_string(),
            cmd: "tempocon".to_string(),
        })?;

        let tempo: Vec<_> = tempo.split(':').collect();

        if tempo.len() != 2 {
            return Err(CmdErr::TempoFormatting {});
        }

        let u = tempo.get(0).unwrap();
        let unit = match *u {
            "b" => TempoUnit::Bpm,
            "m" => TempoUnit::Millis,
            "s" => TempoUnit::Samples,
            _ => {
                return Err(CmdErr::InvalidArg {
                    arg: u.to_string(),
                    cmd: "-t/--tempo".to_string(),
                });
            }
        };

        let int_str = tempo.get(1).unwrap();
        let interval = int_str.parse::<f32>().map_err(|_| CmdErr::InvalidArg {
            arg: tempo[1].to_owned(),
            cmd: "-t/--tempo".to_string(),
        })?;

        let mut tempo_state = TempoRepr::new(self.engine_state.tempo_cons.len());
        tempo_state.init(TempoMode::Context, unit, interval);
        let ts_clone = TempoRepr::clone(&tempo_state);
        self.engine_state
            .tempo_cons
            .insert(name.to_string(), tempo_state);

        Ok(Command::Tc(TcArgs { tempo: ts_clone }))
    }

    // TODO: make able to apply to Group
    // TODO: implement naming Processes
    //       and replace insert("seq".to_string(), ...) with
    //       insert(name, ...)
    fn try_seq(&mut self, args: String) -> CmdResult<Command> {
        let mut args = args.split_whitespace();
        let name = args.next().ok_or(CmdErr::MissingArg {
            arg: "name".to_string(),
            cmd: "seq".to_string(),
        })?;
        let name = name.to_string();

        // default assign to Process:
        // without -t, the seq owns a Process TempoState of 120 BPM
        // (one step per beat)
        let mut tempo: TempoRepr = {
            // TODO: find object? needs to be more generic
            let voice = self.find_voice(name.clone())?;
            TempoRepr::new(voice.proc_tempi.len())
        };
        tempo.init(TempoMode::Process, TempoUnit::Bpm, 120f32);
        let mut period: usize = 4;
        let mut steps: Vec<f32> = Vec::new();
        let mut chance: Vec<f32> = Vec::new();
        let mut jit: Vec<f32> = Vec::new();
        // implement user-defined seed l8r
        let rng = X128P::new(fast_seed());
        // key in the Voice's processes; defaults to the first free
        // of seq, seq1, seq2...
        let mut p_name: Option<String> = None;

        while let Some(arg) = args.next() {
            match arg {
                "-n" | "--name" => {
                    let n_arg = args.next().ok_or(CmdErr::MissingArg {
                        arg: "name".to_string(),
                        cmd: "seq -n".to_string(),
                    })?;
                    p_name = Some(n_arg.to_string());
                }
                "-t" | "--tempo" => {
                    let t_arg = args.next().ok_or(CmdErr::MissingArg {
                        arg: "unit:interval".to_string(),
                        cmd: "seq -t".to_string(),
                    })?;

                    let t_args: Vec<_> = t_arg.split(':').collect();

                    if t_args.len() != 2 {
                        if t_args[0] != "v" {
                            return Err(CmdErr::TempoFormatting {});
                        } else {
                            // refer to Voice's TempoState
                            tempo = {
                                let voice = self.find_voice(name.clone())?;
                                TempoRepr::clone_from(&voice.tempo, voice.idx)
                            };
                            continue;
                        }
                    }

                    let u = t_args.get(0).unwrap();

                    if *u == "c" {
                        // find TempoContext
                        let tc_name = t_args.get(1).unwrap();
                        let tc_name = tc_name.to_string();
                        let tc = self.find_tc(tc_name)?;
                        tempo = TempoRepr::clone_owner(&tc);
                        continue;
                    }

                    if *u == "g" {
                        // find Group
                        let g_name = t_args.get(1).unwrap();
                        let g_name = g_name.to_string();
                        let g = self.find_group(g_name)?;
                        tempo = TempoRepr::clone_from(&g.tempo, g.idx);
                        continue;
                    }

                    // if not referring, then init new TempoState
                    //
                    let unit = match *u {
                        "s" => TempoUnit::Samples,
                        "m" => TempoUnit::Millis,
                        "b" => TempoUnit::Bpm,
                        _ => {
                            return Err(CmdErr::InvalidArg {
                                arg: u.to_string(),
                                cmd: "seq -t".to_string(),
                            });
                        }
                    };

                    let int_str = t_args.get(1).unwrap();
                    let interval = int_str.parse::<f32>().map_err(|_| CmdErr::InvalidArg {
                        arg: int_str.to_string(),
                        cmd: "seq -t".to_string(),
                    })?;

                    tempo.init(TempoMode::Process, unit, interval);
                }
                "-p" | "--period" => {
                    period = args
                        .next()
                        .ok_or(CmdErr::MissingArg {
                            arg: "value".to_string(),
                            cmd: "seq -p".to_string(),
                        })
                        .and_then(|raw| {
                            raw.parse::<usize>().map_err(|_| CmdErr::InvalidArg {
                                arg: raw.to_owned(),
                                cmd: "seq -p".to_string(),
                            })
                        })?;

                    // a period of 0 has no room for steps
                    if period == 0 {
                        return Err(CmdErr::InvalidArg {
                            arg: "0".to_string(),
                            cmd: "seq -p".to_string(),
                        });
                    }
                }
                "-s" | "--steps" => {
                    let s_arg = args.next().ok_or(CmdErr::MissingArg {
                        arg: "value".to_string(),
                        cmd: "seq -s".to_string(),
                    })?;

                    let step_strs: Vec<&str> = s_arg.split(',').collect();

                    // a repeated -s replaces the steps (and the chance/jit
                    // values that were sized to them) rather than appending
                    steps.clear();
                    chance.clear();
                    jit.clear();

                    for step in step_strs {
                        match step.parse::<f32>() {
                            Ok(val) => steps.push(val),
                            Err(_) => {
                                return Err(CmdErr::InvalidArg {
                                    arg: step.to_owned(),
                                    cmd: "seq -s".to_string(),
                                });
                            }
                        }
                    }

                    // set chance and jit Vecs to same len as steps
                    // to avoid panics (jit holds 4 values per step,
                    // see -j); jit defaults to 0 (no jitter)
                    chance.resize(steps.len(), 100f32);
                    jit.resize(steps.len() * 4, 0f32);
                }
                "-c" | "--chance" => {
                    // a value specifies chance for the step
                    //// at the same index as the value
                    // _ is shorthand for 100
                    // n:val specifies chance=val for step=n
                    // a:val sets the same chance=val for all steps
                    // n1-n2:val specifies a chance=val for
                    //// n1-n2 contiguous steps

                    if steps.len() < 1 {
                        return Err(CmdErr::Formatting {
                            err: "Must provide arguments to -s/--steps before -c/--chance or -j/--jitter".to_string()
                        });
                    }

                    let c_arg = args.next().ok_or(CmdErr::MissingArg {
                        arg: "value".to_string(),
                        cmd: "seq -c".to_string(),
                    })?;

                    let c_strs: Vec<&str> = c_arg.split(',').collect();

                    let spec_char = |s: &str| -> Option<char> {
                        for c in s.chars() {
                            match c {
                                '_' => return Some('_'),
                                ':' => return Some(':'),
                                '-' => return Some('-'),
                                _ => continue,
                            }
                        }
                        None
                    };

                    // use chance.len() if too many arguments were provided
                    let len = {
                        if c_strs.len() > chance.len() {
                            chance.len()
                        } else {
                            c_strs.len()
                        }
                    };

                    for i in 0..len {
                        let string = c_strs.get(i).unwrap();
                        match spec_char(string) {
                            Some(c) => {
                                match c {
                                    '_' => chance[i] = 100.0,
                                    ':' => {
                                        let at_index: Vec<&str> = string.split(':').collect();
                                        if at_index.len() != 2 {
                                            return Err(
                                                CmdErr::Formatting {
                                                    err: "Indexed chance arguments must be formatted beat:chance".to_string(),
                                                }
                                            );
                                        }

                                        // get chance first in case index = 'a'
                                        let chance_str = at_index.get(1).unwrap();
                                        let chance_val =
                                            chance_str.parse::<f32>().map_err(|_| {
                                                CmdErr::InvalidArg {
                                                    arg: chance_str.to_string(),
                                                    cmd: "seq -c".to_string(),
                                                }
                                            })?;

                                        let index_str = at_index.get(0).unwrap();

                                        // if index = 'a', set all chance vals to chance_val and continue
                                        if *index_str == "a" {
                                            for i in 0..chance.len() {
                                                chance[i] = chance_val;
                                            }
                                            continue;
                                        }

                                        let index = index_str.parse::<f32>().map_err(|_| {
                                            CmdErr::InvalidArg {
                                                arg: index_str.to_string(),
                                                cmd: "seq -c".to_string(),
                                            }
                                        })?;

                                        let mut found = false;
                                        for i in 0..steps.len() {
                                            let step = *steps.get(i).unwrap();
                                            if index == step {
                                                chance[i] = chance_val;
                                                found = true;
                                                break;
                                            }
                                        }
                                        if !found {
                                            return Err(CmdErr::Formatting {
                                                err: "Invalid index for seq -c".to_string(),
                                            });
                                        }
                                    }
                                    '-' => {
                                        let at_indices: Vec<&str> = string.split(':').collect();
                                        if at_indices.len() != 2 {
                                            return Err(
                                                CmdErr::Formatting {
                                                    err: "Ranged chance arguments must be formatted range:chance".to_string(),
                                                }
                                            );
                                        }

                                        let chance_str = at_indices.get(1).unwrap();
                                        let chance_val =
                                            chance_str.parse::<f32>().map_err(|_| {
                                                CmdErr::InvalidArg {
                                                    arg: chance_str.to_string(),
                                                    cmd: "seq -c".to_string(),
                                                }
                                            })?;

                                        let indices = at_indices.get(0).unwrap();
                                        let indices: Vec<&str> = indices.split('-').collect();
                                        if indices.len() != 2 {
                                            return Err(CmdErr::Formatting {
                                                err: "Ranges must be formatted lower-upper"
                                                    .to_string(),
                                            });
                                        }

                                        let i1_str = indices.get(0).unwrap();
                                        let idx1 = i1_str.parse::<f32>().map_err(|_| {
                                            CmdErr::InvalidArg {
                                                arg: i1_str.to_string(),
                                                cmd: "seq -c".to_string(),
                                            }
                                        })?;
                                        let i2_str = indices.get(1).unwrap();
                                        let idx2 = i2_str.parse::<f32>().map_err(|_| {
                                            CmdErr::InvalidArg {
                                                arg: i2_str.to_string(),
                                                cmd: "seq -c".to_string(),
                                            }
                                        })?;

                                        let mut lower = idx1;
                                        let mut upper = idx2;

                                        if lower > upper {
                                            lower = idx2;
                                            upper = idx1;
                                        }

                                        // steps can be in any order, so check
                                        // whether any step fell in the range
                                        let mut found = false;
                                        for idx in 0..steps.len() {
                                            let step = *steps.get(idx).unwrap();
                                            if step >= lower && step <= upper {
                                                chance[idx] = chance_val;
                                                found = true;
                                            }
                                        }

                                        if !found {
                                            return Err(CmdErr::Formatting {
                                                err: "seq -c range applies to nothing".to_string(),
                                            });
                                        }
                                    }
                                    _ => (),
                                }
                            }
                            // no special chars; just assign value at current index
                            None => {
                                let chance_val =
                                    string.parse::<f32>().map_err(|_| CmdErr::InvalidArg {
                                        arg: string.to_string(),
                                        cmd: "seq -c".to_string(),
                                    })?;
                                chance[i] = chance_val;
                            }
                        }
                    }
                }
                "-j" | "--jitter" => {
                    // a value specifies jitter for the step
                    //// at the same index as the value
                    // _ means no jitter
                    // e|l indicates jitter before=e and after=l the beat
                    //// (of ranges e-0.0 and 0.0-l)
                    // e1-e2|l1-l2 indicate jitter ranges
                    // a single value v is shorthand for v|v
                    // n:e|l specifies jitter=e|l for step=n
                    // a:e|l specifies jitter=e|l for all steps
                    // n1-n2:e1-e2|l1-l2 specifies jitter ranges for
                    //// n1-n2 contiguous steps
                    //
                    // jit stores 4 values per step, at jit[step_idx * 4..]:
                    // [early_min, early_max, late_min, late_max]
                    // (not applied by the engine yet)

                    if steps.len() < 1 {
                        return Err(CmdErr::Formatting {
                            err: "Must provide arguments to -s/--steps before -c/--chance or -j/--jitter".to_string()
                        });
                    }

                    let j_arg = args.next().ok_or(CmdErr::MissingArg {
                        arg: "value".to_string(),
                        cmd: "seq -j".to_string(),
                    })?;

                    // parses one jitter value: _, v, e|l, or e1-e2|l1-l2
                    fn parse_jit(spec: &str) -> CmdResult<[f32; 4]> {
                        if spec == "_" {
                            return Ok([0f32; 4]);
                        }

                        let sides: Vec<&str> = spec.split('|').collect();
                        let (early, late) = match sides.len() {
                            1 => (sides[0], sides[0]),
                            2 => (sides[0], sides[1]),
                            _ => {
                                return Err(CmdErr::Formatting {
                                    err: "Jitter must be formatted early|late".to_string(),
                                });
                            }
                        };

                        let mut out = [0f32; 4];
                        for (i, side) in [early, late].iter().enumerate() {
                            let bounds: Vec<&str> = side.split('-').collect();
                            let mut vals = Vec::<f32>::new();
                            for b in &bounds {
                                let val = b.parse::<f32>().map_err(|_| CmdErr::InvalidArg {
                                    arg: spec.to_string(),
                                    cmd: "seq -j".to_string(),
                                })?;
                                vals.push(val);
                            }

                            let (min, max) = match vals.len() {
                                // e is the range 0.0-e
                                1 => (0f32, vals[0]),
                                2 => (vals[0].min(vals[1]), vals[0].max(vals[1])),
                                _ => {
                                    return Err(CmdErr::Formatting {
                                        err: "Jitter ranges must be formatted lower-upper"
                                            .to_string(),
                                    });
                                }
                            };
                            out[i * 2] = min;
                            out[i * 2 + 1] = max;
                        }

                        Ok(out)
                    }

                    let j_strs: Vec<&str> = j_arg.split(',').collect();

                    for (i, string) in j_strs.iter().enumerate() {
                        let at_index: Vec<&str> = string.split(':').collect();
                        if at_index.len() > 2 {
                            return Err(CmdErr::Formatting {
                                err: "Indexed jitter arguments must be formatted beat:jitter"
                                    .to_string(),
                            });
                        }

                        // no index; just assign value at current index
                        // (extra values are ignored, as with -c)
                        if at_index.len() == 1 {
                            let vals = parse_jit(string)?;
                            if i < steps.len() {
                                jit[i * 4..i * 4 + 4].copy_from_slice(&vals);
                            }
                            continue;
                        }

                        let vals = parse_jit(at_index[1])?;
                        let index_str = at_index[0];

                        let parse_idx = |s: &str| -> CmdResult<f32> {
                            s.parse::<f32>().map_err(|_| CmdErr::InvalidArg {
                                arg: s.to_string(),
                                cmd: "seq -j".to_string(),
                            })
                        };

                        // a: all steps; n1-n2: steps within the range;
                        // n: the step equal to n
                        let (lower, upper) = if index_str == "a" {
                            (f32::NEG_INFINITY, f32::INFINITY)
                        } else if index_str.contains('-') {
                            let indices: Vec<&str> = index_str.split('-').collect();
                            if indices.len() != 2 {
                                return Err(CmdErr::Formatting {
                                    err: "Ranges must be formatted lower-upper".to_string(),
                                });
                            }
                            let idx1 = parse_idx(indices[0])?;
                            let idx2 = parse_idx(indices[1])?;
                            (idx1.min(idx2), idx1.max(idx2))
                        } else {
                            let index = parse_idx(index_str)?;
                            (index, index)
                        };

                        let mut found = false;
                        for idx in 0..steps.len() {
                            if steps[idx] >= lower && steps[idx] <= upper {
                                jit[idx * 4..idx * 4 + 4].copy_from_slice(&vals);
                                found = true;
                            }
                        }
                        if !found {
                            return Err(CmdErr::Formatting {
                                err: "seq -j index applies to nothing".to_string(),
                            });
                        }
                    }
                }
                _ => {
                    return Err(CmdErr::InvalidArg {
                        arg: arg.to_owned(),
                        cmd: "seq".to_string(),
                    });
                }
            }
        }

        // a seq must have steps (no default)
        if steps.is_empty() {
            return Err(CmdErr::MissingArg {
                arg: "-s/--steps".to_string(),
                cmd: "seq".to_string(),
            });
        }

        // TODO: allow for Idx::Group
        let owner = self.find_voice_idx(name.clone())?;
        let voice = self.find_voice(name.clone())?;

        // each Process needs its own key so that processes.len()
        // (and so ProcRepr.idx) matches the engine's Vec<Process>
        let p_name = match p_name {
            Some(n) => {
                if voice.processes.contains_key(&n) {
                    return Err(CmdErr::AlreadyIs {
                        ty: "Process".to_string(),
                        name: n,
                    });
                }
                n
            }
            None => {
                let mut n = "seq".to_string();
                let mut i = 1;
                while voice.processes.contains_key(&n) {
                    n = format!("seq{}", i);
                    i += 1;
                }
                n
            }
        };

        let repr = ProcRepr::new(voice.processes.len(), owner, Some(TempoRepr::clone(&tempo)));
        voice.processes.insert(p_name, repr);
        // push tempo to proc_tempi only if owned by the Process
        if tempo.mode == TempoMode::Process {
            voice
                .proc_tempi
                .insert(voice.proc_tempi.len(), TempoRepr::clone(&tempo));
        }

        let args = SeqArgs {
            idx: owner,
            tempo,
            period,
            steps,
            chance,
            jit,
            rng,
        };

        Ok(Command::Seq(args))
    }

    // StateResults (returned to a CmdResult fn)
    //
    fn parse_type_and_name(&self, args: String, cmd: String) -> StateResult<(String, String)> {
        let mut args = args.split_whitespace();
        let first = args.next().ok_or(StateErr::MissingArg {
            arg: "type and name".to_string(),
            cmd: cmd.clone(),
        })?;
        let second = args.next().ok_or(StateErr::MissingArg {
            arg: "type or name".to_string(),
            cmd: cmd,
        })?;

        Ok((first.to_string(), second.to_string()))
    }

    fn get_idx(&mut self, ty: String, name: String) -> StateResult<Idx> {
        match ty.as_str() {
            "-v" | "--voice" => self.find_voice_idx(name),
            "-g" | "--group" => {
                let g = self.find_group(name)?;
                Ok(Idx::Group(g.idx))
            }
            "-t" | "--tempocontext" => {
                let t = self.find_tc(name)?;
                Ok(Idx::Tempo(t.idx))
            }
            _ => {
                return Err(StateErr::MissingArg {
                    arg: "type".to_string(),
                    cmd: "-v/-g/-t".to_string(),
                });
            }
        }
    }

    fn find_track(&mut self, name: String) -> StateResult<&mut TrackRepr> {
        self.engine_state
            .tracks
            .get_mut(&name)
            .ok_or(StateErr::NoItem {
                ty: "track".to_string(),
                name: name,
            })
    }

    fn find_voice(&mut self, args: String) -> StateResult<&mut VoiceRepr> {
        let args: Vec<&str> = args.split('.').collect();
        if args.len() > 2 {
            return Err(StateErr::Formatting {
                err: "Too many delimiters for format group.voice".to_string(),
            });
        }

        // args will never be 0
        if args.len() == 1 {
            let v_name = args.get(0).unwrap();
            let v_name = v_name.to_string();
            self.engine_state
                .voices
                .get_mut(&v_name)
                .ok_or(StateErr::NoVoice {
                    name: v_name,
                    group: None,
                })
        } else {
            let group = args.get(0).unwrap();
            let group = group.to_string();
            let voice = args.get(1).unwrap();
            let voice = voice.to_string();

            match self.engine_state.groups.get_mut(&group) {
                Some(g) => g.voices.get_mut(&voice).ok_or(StateErr::NoVoice {
                    name: voice,
                    group: Some(group),
                }),
                None => {
                    return Err(StateErr::NoItem {
                        ty: "Group".to_string(),
                        name: group,
                    });
                }
            }
        }
    }

    // engine Idx of a Voice named "voice" or "group.voice"
    fn find_voice_idx(&mut self, name: String) -> StateResult<Idx> {
        let v_idx = self.find_voice(name.clone())?.idx;
        match name.split_once('.') {
            Some((group, _)) => {
                let g_idx = self.find_group(group.to_string())?.idx;
                Ok(Idx::GroupVoice(g_idx, v_idx))
            }
            None => Ok(Idx::Voice(v_idx)),
        }
    }

    fn find_group(&mut self, name: String) -> StateResult<&mut GroupRepr> {
        self.engine_state
            .groups
            .get_mut(&name)
            .ok_or(StateErr::NoItem {
                ty: "Group".to_string(),
                name: name,
            })
    }

    fn find_tc(&mut self, name: String) -> StateResult<&mut TempoRepr> {
        self.engine_state
            .tempo_cons
            .get_mut(&name)
            .ok_or(StateErr::NoItem {
                ty: "TempoContext".to_string(),
                name: name,
            })
    }
}

// results and error handling
//
// ...for Commands
// (user-facing)
//
pub type CmdResult<Command> = Result<Command, CmdErr>;

// ...for states (*Reprs, Idx, args parsed internally, etc.)
// (private, but map directly to CmdErrs)
//
type StateResult<T> = Result<T, StateErr>;

// generate identical enums for CmdErr and StateErr
// and impl conversion from StateErr (internal)
// to CmdErr (user-facing)
//
macro_rules! cmd_errors {
    ( $( $var:ident { $( $arg:ident : $type:ty ),* } ),* $(,)? ) => {
        #[derive(Debug)]
        pub enum CmdErr {
            $(
                $var { $( $arg: $type, )* },
            )*
        }

        // mirrors CmdErr, but not every variant is raised internally
        #[allow(dead_code)]
        #[derive(Debug)]
        enum StateErr {
            $(
                $var { $( $arg: $type, )* },
            )*
        }

        impl From<StateErr> for CmdErr {
            fn from(err: StateErr) -> Self {
                match err {
                    $(
                        StateErr::$var { $( $arg, )* } => {
                            CmdErr::$var { $( $arg, )* }
                        },
                    )*
                }
            }
        }
    }
}

cmd_errors! {
    TempoFormatting {},
    Formatting { err: String }, // generic
    MissingArg { arg: String, cmd: String },
    InvalidArg { arg: String, cmd: String },
    AlreadyIs { ty: String, name: String },
    NoCmd { cmd: String },
    NoItem { ty: String, name: String },
    NoVoice { name: String, group: Option<String> },
}

// display different messages based on error
//
use std::fmt;

impl fmt::Display for CmdErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CmdErr::TempoFormatting {} => {
                write!(f, "-t/--tempo must be formatted as unit:interval")
            }
            CmdErr::Formatting { err } => {
                // verbatim, must explain in context
                write!(f, "{}", err)
            }
            CmdErr::MissingArg { arg, cmd } => {
                write!(f, "Missing {} for '{}'", arg, cmd)
            }
            CmdErr::InvalidArg { arg, cmd } => {
                write!(f, "Invalid argument '{}' for '{}'", arg, cmd)
            }
            CmdErr::AlreadyIs { ty, name } => {
                write!(f, "Already a {} called '{}'", ty, name)
            }
            CmdErr::NoCmd { cmd } => {
                write!(f, "Invalid command '{}'", cmd)
            }
            CmdErr::NoItem { ty, name } => {
                write!(f, "Couldn't find {} '{}'", ty, name)
            }
            CmdErr::NoVoice { name, group } => match group {
                Some(g_name) => write!(f, "Couldn't find Voice '{}' in Group '{}'", name, g_name),
                None => write!(f, "Couldn't find Voice '{}'", name),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_processing::test_util::track;

    fn processor(names: &[&str]) -> CmdProcessor {
        let files = names.iter().map(|n| track(n, 1, 16)).collect();
        CmdProcessor::new(EngineState::new(files, 2))
    }

    fn ok(cp: &mut CmdProcessor, cmd: &str) -> Command {
        match cp.parse(cmd.to_string()) {
            Ok(command) => command,
            Err(error) => panic!("'{}' failed: {}", cmd, error),
        }
    }

    fn err(cp: &mut CmdProcessor, cmd: &str) -> CmdErr {
        match cp.parse(cmd.to_string()) {
            Ok(_) => panic!("'{}' should have failed", cmd),
            Err(error) => error,
        }
    }

    fn seq_tempo(cmd: Command) -> TempoRepr {
        match cmd {
            Command::Seq(args) => args.tempo,
            _ => panic!("not a Seq"),
        }
    }

    fn load_tempo(cmd: Command) -> TempoRepr {
        match cmd {
            Command::Load(args) => args.tempo_repr,
            _ => panic!("not a Load"),
        }
    }

    fn v_idx(cp: &CmdProcessor, name: &str) -> usize {
        cp.engine_state.voices.get(name).unwrap().idx
    }

    // load / unload

    #[test]
    fn load_assigns_increasing_indices() {
        let mut cp = processor(&["kick", "snare"]);
        let t = load_tempo(ok(&mut cp, "load kick"));
        assert!(t.owned);
        assert_eq!(t.mode, TempoMode::TBD);

        let t = load_tempo(ok(&mut cp, "load snare -t b:120"));
        assert!(t.owned);
        assert_eq!(t.mode, TempoMode::Voice);
        assert_eq!(t.unit, TempoUnit::Bpm);
        assert_eq!(t.interval, 120.0);

        assert_eq!(v_idx(&cp, "kick"), 0);
        assert_eq!(v_idx(&cp, "snare"), 1);
    }

    #[test]
    fn load_errors() {
        let mut cp = processor(&["kick"]);
        assert!(matches!(err(&mut cp, "load nope"), CmdErr::NoItem { .. }));
        assert!(matches!(
            err(&mut cp, "load kick -t x:1"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            err(&mut cp, "load kick -t c:nope"),
            CmdErr::NoItem { .. }
        ));
        assert!(matches!(
            err(&mut cp, "load kick -t g:nope"),
            CmdErr::NoItem { .. }
        ));
        assert!(cp.engine_state.voices.is_empty());
        ok(&mut cp, "load kick");
        assert!(matches!(
            err(&mut cp, "load kick"),
            CmdErr::AlreadyIs { .. }
        ));
    }

    #[test]
    fn unload_shifts_later_indices() {
        let mut cp = processor(&["a", "b", "c"]);
        ok(&mut cp, "load a");
        ok(&mut cp, "load b");
        ok(&mut cp, "load c");
        match ok(&mut cp, "unload a") {
            Command::Unload(args) => assert_eq!(args.idx, Idx::Voice(0)),
            _ => panic!("not an Unload"),
        }
        assert!(cp.engine_state.voices.get("a").is_none());
        assert_eq!(v_idx(&cp, "b"), 0);
        assert_eq!(v_idx(&cp, "c"), 1);
        assert!(matches!(err(&mut cp, "unload a"), CmdErr::NoVoice { .. }));
    }

    // tempo references (B15)

    #[test]
    fn seq_voice_tempo_points_at_its_voice() {
        let mut cp = processor(&["kick", "snare"]);
        ok(&mut cp, "load kick");
        ok(&mut cp, "load snare -t b:120");
        let t = seq_tempo(ok(&mut cp, "seq snare -t v -s 0"));
        assert!(!t.owned);
        assert_eq!(t.mode, TempoMode::Voice);
        assert_eq!(t.idx, v_idx(&cp, "snare"));
        assert_eq!(t.idx, 1);
    }

    #[test]
    fn seq_voice_tempo_follows_index_shift() {
        let mut cp = processor(&["kick", "snare"]);
        ok(&mut cp, "load kick");
        ok(&mut cp, "load snare -t b:120");
        ok(&mut cp, "unload kick");
        let t = seq_tempo(ok(&mut cp, "seq snare -t v -s 0"));
        assert_eq!(t.idx, 0);
    }

    #[test]
    fn seq_tbd_voice_tempo_points_at_its_voice() {
        let mut cp = processor(&["kick", "snare"]);
        ok(&mut cp, "load kick");
        ok(&mut cp, "load snare");
        let t = seq_tempo(ok(&mut cp, "seq snare -t v -s 0"));
        assert!(!t.owned);
        assert_eq!(t.mode, TempoMode::TBD);
        assert_eq!(t.idx, 1);
    }

    #[test]
    fn seq_context_tempo_points_at_context() {
        let mut cp = processor(&["a"]);
        ok(&mut cp, "tc c1 b:120");
        ok(&mut cp, "tc c2 b:90");
        ok(&mut cp, "load a");
        let t = seq_tempo(ok(&mut cp, "seq a -t c:c2 -s 0"));
        assert!(!t.owned);
        assert_eq!(t.mode, TempoMode::Context);
        assert_eq!(t.idx, 1);
    }

    #[test]
    fn group_tempo_refs_point_at_their_group() {
        let mut cp = processor(&["a", "b", "c", "d"]);
        ok(&mut cp, "load a");
        ok(&mut cp, "load b");
        ok(&mut cp, "load c");
        ok(&mut cp, "group g1 -v a");
        ok(&mut cp, "group g2 -v b");

        let t = seq_tempo(ok(&mut cp, "seq c -t g:g2 -s 0"));
        assert!(!t.owned);
        assert_eq!(t.mode, TempoMode::Group);
        assert_eq!(t.idx, 1);

        let t = load_tempo(ok(&mut cp, "load d -t g:g1"));
        assert!(!t.owned);
        assert_eq!(t.mode, TempoMode::Group);
        assert_eq!(t.idx, 0);
    }

    // grouping

    fn group_args(cmd: Command) -> GroupArgs {
        match cmd {
            Command::Group(args) => args,
            _ => panic!("not a Group"),
        }
    }

    fn gv_idx(cp: &CmdProcessor, group: &str, voice: &str) -> usize {
        cp.engine_state
            .groups
            .get(group)
            .unwrap()
            .voices
            .get(voice)
            .unwrap()
            .idx
    }

    #[test]
    fn group_voice_order_matches_command_line() {
        let mut cp = processor(&["a", "b", "c"]);
        ok(&mut cp, "load a");
        ok(&mut cp, "load b");
        ok(&mut cp, "load c");
        let args = group_args(ok(&mut cp, "group g -v c,a"));

        // in-group order is c, a
        assert_eq!(gv_idx(&cp, "g", "c"), 0);
        assert_eq!(gv_idx(&cp, "g", "a"), 1);
        // GroupArgs entries are in that same order, holding original idx
        let ids: Vec<usize> = args.vs_fs_ps.iter().map(|(i, _, _)| *i).collect();
        assert_eq!(ids, vec![2, 0]);
        // remaining top-level voice shifted down
        assert_eq!(v_idx(&cp, "b"), 0);
        assert!(cp.engine_state.voices.get("a").is_none());
        assert_eq!(cp.engine_state.groups.get("g").unwrap().idx, 0);
    }

    // addressing grouped voices (B04)

    fn grouped(names: &[&str], group: &str) -> CmdProcessor {
        let mut cp = processor(names);
        for n in names {
            ok(&mut cp, &format!("load {}", n));
        }
        ok(&mut cp, group);
        cp
    }

    #[test]
    fn velocity_targets_group_voice() {
        let mut cp = grouped(&["a", "b", "c"], "group g -v a,b");
        match ok(&mut cp, "velocity g.b 2") {
            Command::Velocity(args) => {
                assert_eq!(args.idx, Idx::GroupVoice(0, 1));
                assert_eq!(args.val, 2.0);
            }
            _ => panic!("not a Velocity"),
        }
        match ok(&mut cp, "velocity c -1.5") {
            Command::Velocity(args) => {
                assert_eq!(args.idx, Idx::Voice(0));
                assert_eq!(args.val, -1.5);
            }
            _ => panic!("not a Velocity"),
        }
        assert!(matches!(
            err(&mut cp, "velocity c"),
            CmdErr::MissingArg { .. }
        ));
        assert!(matches!(
            err(&mut cp, "velocity c x"),
            CmdErr::InvalidArg { .. }
        ));
    }

    #[test]
    fn start_targets_group_voice() {
        let mut cp = grouped(&["a", "b"], "group g -v a,b");
        for (cmd, want) in [
            ("start -v g.a", Idx::GroupVoice(0, 0)),
            ("pause -v g.b", Idx::GroupVoice(0, 1)),
            ("start -g g", Idx::Group(0)),
        ] {
            let idx = match ok(&mut cp, cmd) {
                Command::Start(args) => args.idx,
                Command::Pause(args) => args.idx,
                _ => panic!("unexpected Command"),
            };
            assert_eq!(idx, want, "{}", cmd);
        }
    }

    #[test]
    fn missing_group_voice_errors() {
        let mut cp = grouped(&["a"], "group g -v a");
        match err(&mut cp, "start -v g.zzz") {
            CmdErr::NoVoice { name, group } => {
                assert_eq!(name, "zzz");
                assert_eq!(group, Some("g".to_string()));
            }
            other => panic!("wrong error {:?}", other),
        }
        assert!(matches!(
            err(&mut cp, "start -v nope.a"),
            CmdErr::NoItem { .. }
        ));
        assert!(matches!(
            err(&mut cp, "start -v a"),
            CmdErr::NoVoice { group: None, .. }
        ));
        assert!(matches!(
            err(&mut cp, "start -v g.a.b"),
            CmdErr::Formatting { .. }
        ));
    }

    #[test]
    fn seq_targets_group_voice() {
        let mut cp = grouped(&["a", "b"], "group g -v a,b");
        match ok(&mut cp, "seq g.b -s 0") {
            Command::Seq(args) => assert_eq!(args.idx, Idx::GroupVoice(0, 1)),
            _ => panic!("not a Seq"),
        }
        let gv = cp
            .engine_state
            .groups
            .get("g")
            .unwrap()
            .voices
            .get("b")
            .unwrap();
        assert_eq!(gv.processes.len(), 1);
    }

    // group tempo (B12)

    #[test]
    fn group_default_tempo_is_owned() {
        let mut cp = processor(&["a"]);
        ok(&mut cp, "load a");
        let args = group_args(ok(&mut cp, "group g -v a"));
        assert!(args.tempo.owned);
        assert_eq!(args.tempo.mode, TempoMode::Group);
        assert_eq!(args.tempo.unit, TempoUnit::Bpm);
        assert_eq!(args.tempo.interval, 240.0);

        let mut cp = processor(&["a"]);
        ok(&mut cp, "load a");
        let args = group_args(ok(&mut cp, "group g -t m:250 -v a"));
        assert!(args.tempo.owned);
        assert_eq!(args.tempo.mode, TempoMode::Group);
        assert_eq!(args.tempo.unit, TempoUnit::Millis);
        assert_eq!(args.tempo.interval, 250.0);
    }

    #[test]
    fn group_shares_named_context() {
        let mut cp = processor(&["a"]);
        ok(&mut cp, "tc other b:60");
        ok(&mut cp, "tc clk b:120");
        ok(&mut cp, "load a");
        let args = group_args(ok(&mut cp, "group g -t c:clk -v a"));
        assert!(!args.tempo.owned);
        assert_eq!(args.tempo.mode, TempoMode::Context);
        assert_eq!(args.tempo.idx, 1);
        // the TBD voice follows the group, i.e. the context
        let (_, follows, _) = args.vs_fs_ps[0];
        assert!(follows);
        let a = cp
            .engine_state
            .groups
            .get("g")
            .unwrap()
            .voices
            .get("a")
            .unwrap();
        assert_eq!(a.tempo.mode, TempoMode::Context);
        assert_eq!(a.tempo.idx, 1);
    }

    #[test]
    fn group_tempo_errors() {
        let mut cp = processor(&["a"]);
        ok(&mut cp, "tc clk b:120");
        ok(&mut cp, "load a");
        assert!(matches!(
            err(&mut cp, "group g -t c:nope -v a"),
            CmdErr::NoItem { .. }
        ));
        assert!(matches!(
            err(&mut cp, "group g -t c"),
            CmdErr::MissingArg { .. }
        ));
        assert!(matches!(
            err(&mut cp, "group g -t x:1"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            err(&mut cp, "group g -t b"),
            CmdErr::MissingArg { .. }
        ));
        assert!(matches!(
            err(&mut cp, "group g -t b:fast"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            err(&mut cp, "group g -t"),
            CmdErr::MissingArg { .. }
        ));
        assert!(matches!(err(&mut cp, "group"), CmdErr::MissingArg { .. }));
        assert!(matches!(
            err(&mut cp, "group g -x"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(cp.engine_state.groups.is_empty());
    }

    // flags / process ids stay with their voice (B13)

    const PERMS: [&str; 6] = ["a,b,c", "a,c,b", "b,a,c", "b,c,a", "c,a,b", "c,b,a"];

    #[test]
    fn group_flags_follow_their_voice() {
        // HashMap order varies per map, so repeat with fresh processors
        for _ in 0..20 {
            for perm in PERMS {
                let mut cp = processor(&["a", "b", "c"]);
                ok(&mut cp, "load a");
                ok(&mut cp, "load b -t b:90");
                ok(&mut cp, "load c");
                ok(&mut cp, "seq a -t v -s 0");
                ok(&mut cp, "seq c -t v -s 0");
                let args = group_args(ok(&mut cp, &format!("group g -v {}", perm)));

                for (pos, (orig, follows, p_ids)) in args.vs_fs_ps.iter().enumerate() {
                    // a=0, b=1, c=2
                    let name = ["a", "b", "c"][*orig];
                    assert_eq!(*follows, name != "b", "{} in {}", name, perm);
                    match name {
                        "b" => assert!(p_ids.is_empty(), "{} in {}", name, perm),
                        _ => assert_eq!(p_ids, &vec![0], "{} in {}", name, perm),
                    }
                    assert_eq!(gv_idx(&cp, "g", name), pos);
                }

                let g = cp.engine_state.groups.get("g").unwrap();
                assert_eq!(g.voices.get("a").unwrap().tempo.mode, TempoMode::Group);
                assert_eq!(g.voices.get("b").unwrap().tempo.mode, TempoMode::Voice);
                assert_eq!(g.voices.get("c").unwrap().tempo.mode, TempoMode::Group);
            }
        }
    }

    // only voice-following processes move to the group tempo (B14)

    #[test]
    fn group_keeps_owned_process_tempos() {
        let mut cp = processor(&["a"]);
        ok(&mut cp, "load a");
        ok(&mut cp, "seq a -t b:140 -s 0");
        let args = group_args(ok(&mut cp, "group g -v a"));
        let (_, follows, p_ids) = &args.vs_fs_ps[0];
        assert!(*follows);
        assert!(p_ids.is_empty());
        let a = cp
            .engine_state
            .groups
            .get("g")
            .unwrap()
            .voices
            .get("a")
            .unwrap();
        let t = a.processes.get("seq").unwrap().tempo.as_ref().unwrap();
        assert_eq!(t.mode, TempoMode::Process);
    }

    #[test]
    fn group_moves_voice_following_processes() {
        let mut cp = processor(&["a"]);
        ok(&mut cp, "load a");
        ok(&mut cp, "seq a -t v -s 0");
        let args = group_args(ok(&mut cp, "group g -v a"));
        let (_, _, p_ids) = &args.vs_fs_ps[0];
        assert_eq!(p_ids, &vec![0]);
        let a = cp
            .engine_state
            .groups
            .get("g")
            .unwrap()
            .voices
            .get("a")
            .unwrap();
        let t = a.processes.get("seq").unwrap().tempo.as_ref().unwrap();
        assert_eq!(t.mode, TempoMode::Group);
        assert!(!t.owned);
        assert_eq!(t.idx, 0);
    }

    // failed commands don't touch the mirror (B16)

    fn loaded(names: &[&str]) -> CmdProcessor {
        let mut cp = processor(names);
        for n in names {
            ok(&mut cp, &format!("load {}", n));
        }
        cp
    }

    fn assert_untouched(cp: &CmdProcessor, names: &[&str]) {
        assert!(cp.engine_state.groups.is_empty());
        assert_eq!(cp.engine_state.voices.len(), names.len());
        for (i, n) in names.iter().enumerate() {
            assert_eq!(v_idx(cp, n), i, "{}", n);
        }
    }

    #[test]
    fn failed_group_leaves_mirror_unchanged() {
        let names = ["a", "b", "c"];
        for cmd in [
            "group g -v a,nope",
            "group g -v nope,b",
            "group g -v a -t x:1",
            "group g -v a -t c:nope",
            "group g -v c -v nope",
            "group g -v a,a",
            "group g -v a -v a",
            "group g -v b -x",
            "group g -v",
        ] {
            let mut cp = loaded(&names);
            err(&mut cp, cmd);
            assert_untouched(&cp, &names);
        }
    }

    #[test]
    fn group_unknown_voice_is_no_voice() {
        let mut cp = loaded(&["a"]);
        match err(&mut cp, "group g -v a,nope") {
            CmdErr::NoVoice { name, group } => {
                assert_eq!(name, "nope");
                assert_eq!(group, None);
            }
            other => panic!("wrong error {:?}", other),
        }
    }

    #[test]
    fn group_with_several_voice_flags() {
        let mut cp = loaded(&["a", "b", "c", "d"]);
        let args = group_args(ok(&mut cp, "group g -v c -v a"));
        let ids: Vec<usize> = args.vs_fs_ps.iter().map(|(i, _, _)| *i).collect();
        assert_eq!(ids, vec![2, 0]);
        assert_eq!(gv_idx(&cp, "g", "c"), 0);
        assert_eq!(gv_idx(&cp, "g", "a"), 1);
        assert_eq!(v_idx(&cp, "b"), 0);
        assert_eq!(v_idx(&cp, "d"), 1);
    }

    #[test]
    fn full_queue_leaves_mirror_unchanged() {
        let mut cp = processor(&["a"]);
        let queue = CmdQueue::new(2); // holds one Command
        assert!(!queue.is_full());

        cp.submit("tc clk b:120".to_string(), &queue).unwrap();
        assert!(queue.is_full());

        assert!(cp.submit("load a".to_string(), &queue).is_err());
        assert!(cp.engine_state.voices.is_empty());

        assert!(queue.try_pop().is_some());
        assert!(!queue.is_full());
        cp.submit("load a".to_string(), &queue).unwrap();
        assert_eq!(v_idx(&cp, "a"), 0);
        assert!(matches!(queue.try_pop(), Some(Command::Load(_))));
        assert!(queue.try_pop().is_none());
    }

    #[test]
    fn submit_reports_parse_errors() {
        let mut cp = processor(&["a"]);
        let queue = CmdQueue::new(4);
        let error = cp.submit("load nope".to_string(), &queue).unwrap_err();
        assert!(error.contains("nope"));
        assert!(queue.try_pop().is_none());
    }

    // duplicate names (B17)

    fn tc_tempo(cmd: Command) -> TempoRepr {
        match cmd {
            Command::Tc(args) => args.tempo,
            _ => panic!("not a Tc"),
        }
    }

    #[test]
    fn tc_parses_tempo() {
        let mut cp = processor(&[]);
        let t = tc_tempo(ok(&mut cp, "tc a b:120"));
        assert!(t.owned);
        assert_eq!(t.mode, TempoMode::Context);
        assert_eq!(t.unit, TempoUnit::Bpm);
        assert_eq!(t.interval, 120.0);
        assert_eq!(t.idx, 0);
        let t = tc_tempo(ok(&mut cp, "tempocon b m:500"));
        assert_eq!(t.unit, TempoUnit::Millis);
        assert_eq!(t.idx, 1);

        assert!(matches!(err(&mut cp, "tc"), CmdErr::MissingArg { .. }));
        assert!(matches!(err(&mut cp, "tc c"), CmdErr::MissingArg { .. }));
        assert!(matches!(
            err(&mut cp, "tc c b"),
            CmdErr::TempoFormatting { .. }
        ));
        assert!(matches!(
            err(&mut cp, "tc c x:1"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            err(&mut cp, "tc c b:fast"),
            CmdErr::InvalidArg { .. }
        ));
        assert_eq!(cp.engine_state.tempo_cons.len(), 2);
    }

    #[test]
    fn duplicate_tc_is_rejected() {
        let mut cp = processor(&[]);
        ok(&mut cp, "tc a b:120");
        assert!(matches!(
            err(&mut cp, "tc a b:90"),
            CmdErr::AlreadyIs { .. }
        ));
        let a = cp.engine_state.tempo_cons.get("a").unwrap();
        assert_eq!(a.interval, 120.0);
        assert_eq!(cp.engine_state.tempo_cons.len(), 1);

        // later contexts get their engine Vec position
        let t = tc_tempo(ok(&mut cp, "tc b b:60"));
        assert_eq!(t.idx, 1);
        assert_eq!(cp.engine_state.tempo_cons.get("b").unwrap().idx, 1);
    }

    #[test]
    fn duplicate_group_is_rejected() {
        let mut cp = loaded(&["a", "b", "c"]);
        ok(&mut cp, "group g -v a");
        assert!(matches!(
            err(&mut cp, "group g -v b"),
            CmdErr::AlreadyIs { .. }
        ));
        // b wasn't consumed
        assert_eq!(v_idx(&cp, "b"), 0);
        assert_eq!(v_idx(&cp, "c"), 1);
        assert_eq!(cp.engine_state.groups.len(), 1);
        assert!(
            cp.engine_state
                .groups
                .get("g")
                .unwrap()
                .voices
                .contains_key("a")
        );

        ok(&mut cp, "group h -v b");
        assert_eq!(cp.engine_state.groups.get("h").unwrap().idx, 1);
    }
}

#[cfg(test)]
mod seq_tests {
    use super::*;

    fn processor() -> CmdProcessor {
        let kick = AudioFile::new("kick", "wav", 48000, 1, 16, vec![0.0; 16]);
        let mut cp = CmdProcessor::new(EngineState::new(vec![kick], 2));
        assert!(cp.parse("load kick".to_string()).is_ok());
        cp
    }

    fn seq(cp: &mut CmdProcessor, cmd: &str) -> CmdResult<SeqArgs> {
        match cp.parse(cmd.to_string())? {
            Command::Seq(args) => Ok(args),
            _ => panic!("'{}' didn't parse to Command::Seq", cmd),
        }
    }

    fn seq_ok(cmd: &str) -> SeqArgs {
        let mut cp = processor();
        match seq(&mut cp, cmd) {
            Ok(args) => args,
            Err(e) => panic!("'{}' failed: {}", cmd, e),
        }
    }

    fn seq_err(cmd: &str) -> CmdErr {
        let mut cp = processor();
        match seq(&mut cp, cmd) {
            Ok(_) => panic!("'{}' should have failed", cmd),
            Err(e) => e,
        }
    }

    #[test]
    fn seq_default_tempo_is_owned_process() {
        let args = seq_ok("seq kick -s 0,1");
        assert!(args.tempo.owned);
        assert_eq!(args.tempo.mode, TempoMode::Process);
        assert_eq!(args.tempo.unit, TempoUnit::Bpm);
        assert_eq!(args.tempo.interval, 120.0);
    }

    #[test]
    fn seq_tempo_flag() {
        let args = seq_ok("seq kick -t m:250 -s 0");
        assert!(args.tempo.owned);
        assert_eq!(args.tempo.mode, TempoMode::Process);
        assert_eq!(args.tempo.unit, TempoUnit::Millis);
        assert_eq!(args.tempo.interval, 250.0);

        let args = seq_ok("seq kick -t v -s 0");
        assert!(!args.tempo.owned);

        assert!(matches!(
            seq_err("seq kick -t x:1 -s 0"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -t b:fast -s 0"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -t b -s 0"),
            CmdErr::TempoFormatting {}
        ));
    }

    #[test]
    fn seq_period_and_steps() {
        let args = seq_ok("seq kick -p 8 -s 0,1.5,3");
        assert_eq!(args.period, 8);
        assert_eq!(args.steps, vec![0.0, 1.5, 3.0]);
        assert_eq!(args.chance.len(), 3);
        assert_eq!(args.jit.len(), 3 * 4);

        let args = seq_ok("seq kick -s 0");
        assert_eq!(args.period, 4);

        assert!(matches!(
            seq_err("seq kick -p x -s 0"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,x"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(seq_err("seq kick -s"), CmdErr::MissingArg { .. }));
    }

    #[test]
    fn seq_unknown_flag_and_voice() {
        assert!(matches!(
            seq_err("seq kick -s 0 -z"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(seq_err("seq snare -s 0"), CmdErr::NoVoice { .. }));
        assert!(matches!(seq_err("seq"), CmdErr::MissingArg { .. }));
    }
    #[test]
    fn seq_requires_steps() {
        assert!(matches!(
            seq_err("seq kick -t b:120"),
            CmdErr::MissingArg { .. }
        ));
        assert!(matches!(seq_err("seq kick"), CmdErr::MissingArg { .. }));

        // a rejected seq leaves the mirror untouched
        let mut cp = processor();
        assert!(seq(&mut cp, "seq kick -t b:120").is_err());
        let voice = cp.find_voice("kick".to_string()).unwrap();
        assert_eq!(voice.processes.len(), 0);
        assert_eq!(voice.proc_tempi.len(), 0);
    }

    #[test]
    fn seq_rejects_zero_period() {
        assert!(matches!(
            seq_err("seq kick -p 0 -s 0"),
            CmdErr::InvalidArg { .. }
        ));
        assert_eq!(seq_ok("seq kick -p 1 -s 0").period, 1);
    }
    #[test]
    fn seq_chance_at_index() {
        assert_eq!(
            seq_ok("seq kick -s 0,1,2 -c 2:50").chance,
            vec![100.0, 100.0, 50.0]
        );
        assert_eq!(
            seq_ok("seq kick -s 0,1,2 -c 1:50").chance,
            vec![100.0, 50.0, 100.0]
        );
        assert_eq!(
            seq_ok("seq kick -s 0,1,2 -c 0:50").chance,
            vec![50.0, 100.0, 100.0]
        );
        assert_eq!(
            seq_ok("seq kick -s 0,1.5 -c 1.5:25").chance,
            vec![100.0, 25.0]
        );
        assert!(matches!(
            seq_err("seq kick -s 0,1,2 -c 5:50"),
            CmdErr::Formatting { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1,2 -c x:50"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1,2 -c 1:x"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1,2 -c 1:2:3"),
            CmdErr::Formatting { .. }
        ));
    }

    #[test]
    fn seq_chance_all_positional_and_default() {
        assert_eq!(
            seq_ok("seq kick -s 0,1,2").chance,
            vec![100.0, 100.0, 100.0]
        );
        assert_eq!(
            seq_ok("seq kick -s 0,1,2 -c a:30").chance,
            vec![30.0, 30.0, 30.0]
        );
        assert_eq!(
            seq_ok("seq kick -s 0,1,2 -c 10,20").chance,
            vec![10.0, 20.0, 100.0]
        );
        assert_eq!(
            seq_ok("seq kick -s 0,1,2 -c 10,_,30").chance,
            vec![10.0, 100.0, 30.0]
        );
        // extra values are ignored
        assert_eq!(
            seq_ok("seq kick -s 0,1 -c 10,20,30").chance,
            vec![10.0, 20.0]
        );
        assert!(matches!(
            seq_err("seq kick -s 0,1 -c x"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1 -c"),
            CmdErr::MissingArg { .. }
        ));
        // -c needs steps first
        assert!(matches!(
            seq_err("seq kick -c 50 -s 0,1"),
            CmdErr::Formatting { .. }
        ));
    }
    #[test]
    fn seq_chance_range() {
        let s = "seq kick -s 0,1,2,3 -c";
        assert_eq!(
            seq_ok(&format!("{} 1-3:20", s)).chance,
            vec![100.0, 20.0, 20.0, 20.0]
        );
        assert_eq!(
            seq_ok(&format!("{} 3-1:20", s)).chance,
            vec![100.0, 20.0, 20.0, 20.0]
        );
        assert_eq!(
            seq_ok(&format!("{} 0-1:20", s)).chance,
            vec![20.0, 20.0, 100.0, 100.0]
        );
        assert_eq!(
            seq_ok(&format!("{} 2-9:20", s)).chance,
            vec![100.0, 100.0, 20.0, 20.0]
        );
        assert_eq!(
            seq_ok(&format!("{} 1-1:20", s)).chance,
            vec![100.0, 20.0, 100.0, 100.0]
        );
        assert!(matches!(
            seq_err(&format!("{} 5-9:20", s)),
            CmdErr::Formatting { .. }
        ));
        assert!(matches!(
            seq_err(&format!("{} 1-x:20", s)),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err(&format!("{} 1-3", s)),
            CmdErr::Formatting { .. }
        ));
        assert!(matches!(
            seq_err(&format!("{} 1-2-3:20", s)),
            CmdErr::Formatting { .. }
        ));
    }
    #[test]
    fn seq_jitter_default_and_none() {
        assert_eq!(seq_ok("seq kick -s 0,1").jit, vec![0.0; 8]);
        assert_eq!(seq_ok("seq kick -s 0,1 -j _").jit, vec![0.0; 8]);
        assert_eq!(seq_ok("seq kick -s 0,1 -j 5|5 -j _,_").jit, vec![0.0; 8]);
    }

    #[test]
    fn seq_jitter_values() {
        // jit[step * 4..] = [early_min, early_max, late_min, late_max]
        assert_eq!(
            seq_ok("seq kick -s 0,1 -j 5|10").jit,
            vec![0.0, 5.0, 0.0, 10.0, 0.0, 0.0, 0.0, 0.0]
        );
        assert_eq!(
            seq_ok("seq kick -s 0,1 -j 3,2-4|8-6").jit,
            vec![0.0, 3.0, 0.0, 3.0, 2.0, 4.0, 6.0, 8.0]
        );
        // extra values are ignored
        assert_eq!(seq_ok("seq kick -s 0 -j 1,2").jit, vec![0.0, 1.0, 0.0, 1.0]);
        // -j consumes its value, so later flags still parse
        let args = seq_ok("seq kick -s 0,1 -j 5|5 -c 1:50");
        assert_eq!(args.chance, vec![100.0, 50.0]);
    }

    #[test]
    fn seq_jitter_indexed() {
        let j = |s: &str| seq_ok(&format!("seq kick -s 0,1,2 -j {}", s)).jit;
        let none = [0.0f32; 4];
        let v = [0.0f32, 5.0, 0.0, 10.0];
        let cat = |a: [[f32; 4]; 3]| a.concat();

        assert_eq!(j("1:5|10"), cat([none, v, none]));
        assert_eq!(j("a:5|10"), cat([v, v, v]));
        assert_eq!(j("1-2:5|10"), cat([none, v, v]));
        assert_eq!(j("2-1:5|10"), cat([none, v, v]));
        assert_eq!(j("0:5|10,2:5|10"), cat([v, none, v]));

        assert!(matches!(
            seq_err("seq kick -s 0,1,2 -j 7:5|10"),
            CmdErr::Formatting { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1,2 -j 5-9:5|10"),
            CmdErr::Formatting { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1,2 -j x:5|10"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1,2 -j 1:2:3"),
            CmdErr::Formatting { .. }
        ));
    }

    #[test]
    fn seq_jitter_errors() {
        assert!(matches!(
            seq_err("seq kick -s 0,1 -j"),
            CmdErr::MissingArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -j 5|5 -s 0,1"),
            CmdErr::Formatting { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1 -j x"),
            CmdErr::InvalidArg { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1 -j 1|2|3"),
            CmdErr::Formatting { .. }
        ));
        assert!(matches!(
            seq_err("seq kick -s 0,1 -j 1-2-3"),
            CmdErr::Formatting { .. }
        ));
    }
    fn proc_idx(cp: &mut CmdProcessor, p_name: &str) -> usize {
        let voice = cp.find_voice("kick".to_string()).unwrap();
        voice.processes.get(p_name).unwrap().idx
    }

    #[test]
    fn seq_default_names_are_unique() {
        let mut cp = processor();
        for _ in 0..3 {
            assert!(seq(&mut cp, "seq kick -s 0").is_ok());
        }
        assert_eq!(
            cp.find_voice("kick".to_string()).unwrap().processes.len(),
            3
        );
        assert_eq!(proc_idx(&mut cp, "seq"), 0);
        assert_eq!(proc_idx(&mut cp, "seq1"), 1);
        assert_eq!(proc_idx(&mut cp, "seq2"), 2);
        // each owns a Process tempo
        assert_eq!(
            cp.find_voice("kick".to_string()).unwrap().proc_tempi.len(),
            3
        );
    }

    #[test]
    fn seq_named() {
        let mut cp = processor();
        assert!(seq(&mut cp, "seq kick -n hats -s 0").is_ok());
        assert!(seq(&mut cp, "seq kick --name seq -s 0").is_ok());
        // default skips names already taken
        assert!(seq(&mut cp, "seq kick -s 0").is_ok());
        assert_eq!(proc_idx(&mut cp, "hats"), 0);
        assert_eq!(proc_idx(&mut cp, "seq"), 1);
        assert_eq!(proc_idx(&mut cp, "seq1"), 2);

        match seq(&mut cp, "seq kick -n hats -s 0") {
            Err(CmdErr::AlreadyIs { ty, name }) => {
                assert_eq!(ty, "Process");
                assert_eq!(name, "hats");
            }
            _ => panic!("duplicate seq name should fail"),
        }
        // the failed seq didn't touch the mirror
        assert_eq!(
            cp.find_voice("kick".to_string()).unwrap().processes.len(),
            3
        );
        assert_eq!(
            cp.find_voice("kick".to_string()).unwrap().proc_tempi.len(),
            3
        );

        assert!(matches!(
            seq_err("seq kick -s 0 -n"),
            CmdErr::MissingArg { .. }
        ));
    }

    #[test]
    fn repeated_steps_flag_replaces() {
        let args = seq_ok("seq kick -s 0,1,2 -c 50 -s 0,2");
        assert_eq!(args.steps, vec![0.0, 2.0]);
        assert_eq!(args.chance, vec![100.0, 100.0]);
        assert_eq!(args.jit.len(), 8);
    }

    #[test]
    fn chance_range_with_unsorted_steps() {
        let args = seq_ok("seq kick -s 3,0 -c 2-5:40");
        assert_eq!(args.chance, vec![40.0, 100.0]);
        assert!(matches!(
            seq_err("seq kick -s 3,0 -c 4-5:40"),
            CmdErr::Formatting { .. }
        ));
    }
}
