//! Voices: creating them, transport and playback rate.

use super::buffer::SetSourceArgs;
use super::errors::{CmdErr, CmdResult};
use super::fx::FxRepr;
use super::lang::ast::ActionWord;
use super::procs::seq::RecvSpec;
use super::reprs::{
    BufferId, EngineState, GroupId, ProcId, ProcOwner, Target, TempoId, TempoLink, TempoOwner,
    TrackId, VoiceId, alloc,
};
use super::tempo::{ClockRun, TempoChoice, TempoUse};
use super::track::engine_rate;
use super::wave::WaveRepr;
use super::{BatchArgs, CmdProcessor, Command};
use crate::audio_processing::engine::default_bounce;
use crate::audio_processing::engine::shift::{VoiceShifters, default_window};
use crate::audio_processing::engine::{
    BufData, ChoirMode, LoopMode, MAX_VOICE_CHANNELS, VoiceParam, VoiceSource,
};
use crate::audio_processing::processes::LfoTarget;
use std::collections::BTreeMap;
use std::sync::Arc;

/// What a voice plays, in the mirror.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceSrc {
    Track(TrackId),
    Buffer(BufferId),
    /// Waves only: it has no recorded source (WAVa).
    Waves,
    /// The buffer it played was removed: it is silent.
    Gone,
}

/// A Voice in the mirror.
#[derive(Clone)]
pub struct VoiceRepr {
    #[allow(dead_code)] // the map key; kept so a repr names itself
    pub(crate) id: VoiceId,
    pub(crate) name: String,
    /// What it plays.
    pub(crate) source: VoiceSrc,
    pub(crate) group: Option<GroupId>,
    pub(crate) tempo: TempoId,
    pub(crate) link: TempoLink,
    /// Its period in ticks of its clock (`in N`, G5).
    pub(crate) period: u32,
    /// The base value of every parameter, as the engine holds it (`slice`
    /// is 0-based here; the language counts from 1).
    pub(crate) params: [f32; VoiceParam::COUNT],
    pub(crate) loop_mode: LoopMode,
    /// How many slices the track is cut into; 0 = not sliced.
    pub(crate) slices: u16,
    /// How many instances it sings with (1..=8).
    pub(crate) choir: u8,
    pub(crate) choir_mode: ChoirMode,
    /// Which engine plays `pitch=`/`stretch=` (REQ-099).
    pub(crate) shift: ShiftMode,
    /// How a plain head reads the source (`resample=`, REQ-143).
    pub(crate) resample: ResampleMode,
    /// The spectral shifter's window in frames (`window=`, REQ-105); only
    /// the shifters read it.
    pub(crate) window: usize,
    /// `bounce=`: the fade at a ping-pong turnaround, in frames (0 = off;
    /// REQ-106). Starts at the engine's default.
    pub(crate) bounce: u32,
    /// The shape `(channels, instances, window)` of the shifters sent to the
    /// engine so far; `None` before any. They stay when a voice stops
    /// wanting them.
    pub(crate) shifters_built: Option<(usize, usize, usize)>,
    /// Its processes by name.
    pub(crate) procs: BTreeMap<String, ProcId>,
    /// Its effects, in chain order (FXa).
    pub(crate) fx: Vec<FxRepr>,
    /// How many effects the engine's chain has room for before an insert
    /// must carry a bigger `Vec`.
    pub(crate) fx_cap: usize,
    /// Its waves, in the order they were made (WAVa).
    pub(crate) waves: Vec<WaveRepr>,
    /// The engine's `VoiceState.active`: started or resumed and not since
    /// stopped or paused (a paused group still leaves it set). Kept so the
    /// mirror knows which clocks run (CLKG).
    pub(crate) active: bool,
}

impl EngineState {
    /// Whether voice `v` should have spectral shifters (REQ-099): its
    /// `shift` is spectral, and it uses `pitch=` or `stretch=` or may start
    /// to. That is so when any of these holds:
    /// - the base pitch or stretch is off its default;
    /// - an lfo or env owned by the voice, or by its group, targets pitch
    ///   or stretch;
    /// - a `melo` receiver targets the voice (it sets the pitch).
    pub(crate) fn shifters_wanted(&self, v: VoiceId) -> bool {
        let Some(repr) = self.voices.get(&v) else {
            return false;
        };
        if repr.shift != ShiftMode::Spectral {
            return false;
        }
        let moved = |p: VoiceParam| repr.params[p.index()] != p.default_value();
        if moved(VoiceParam::Pitch) || moved(VoiceParam::Stretch) {
            return true;
        }
        let modulates =
            |t: LfoTarget| matches!(t, LfoTarget::Voice(VoiceParam::Pitch | VoiceParam::Stretch));
        let mine = |owner: ProcOwner| {
            owner == ProcOwner::Voice(v) || repr.group.is_some_and(|g| owner == ProcOwner::Group(g))
        };
        self.procs.values().any(|p| {
            (mine(p.owner)
                && (p.lfo.as_ref().is_some_and(|l| modulates(l.target))
                    || p.env.as_ref().is_some_and(|e| modulates(e.target))))
                || p.seq
                    .recv
                    .iter()
                    .any(|r| matches!(r, RecvSpec::Melo { target, .. } if *target == v))
        })
    }

    /// The shifters voice `v` needs: `(channels, instances, window)`, with channels
    /// as the engine plays them (at most 8). `None` while it doesn't want
    /// any, or its source is gone.
    pub(crate) fn shifters_shape(&self, v: VoiceId) -> Option<(usize, usize, usize)> {
        if !self.shifters_wanted(v) {
            return None;
        }
        let repr = self.voices.get(&v)?;
        let channels = match repr.source {
            VoiceSrc::Track(id) => self.tracks.values().find(|t| t.id == id)?.channels as usize,
            VoiceSrc::Buffer(id) => usize::from(self.buffer_by_id(id)?.data.channels),
            // the waves are mono, and no shifter reads them
            VoiceSrc::Waves | VoiceSrc::Gone => return None,
        };
        let channels = channels.min(MAX_VOICE_CHANNELS);
        (channels > 0).then_some((channels, usize::from(repr.choir), repr.window))
    }
}

impl CmdProcessor {
    /// The `SetShifters` commands the mirror owes the engine, after a script
    /// lowered: one for each voice that wants shifters of a shape it
    /// doesn't have. Builds them (allocating) and records the shape.
    /// Shifters stay when a voice stops wanting them, which keeps memory
    /// flat when settings are toggled. Nothing is sent if the script
    /// fails: the caller restores the mirror.
    pub(in crate::audio_processing::commands) fn owed_shifters(
        &mut self,
    ) -> Vec<(VoiceId, Command)> {
        self.owed_shifters_of(false)
    }

    /// `owed_shifters` for the voices a waiting line makes and the engine
    /// doesn't have yet (they go with the statement that makes them
    /// want shifters); `owed_shifters` leaves those out.
    pub(in crate::audio_processing::commands) fn owed_shifters_pending(
        &mut self,
    ) -> Vec<(VoiceId, Command)> {
        self.owed_shifters_of(true)
    }

    fn owed_shifters_of(&mut self, pending: bool) -> Vec<(VoiceId, Command)> {
        let rate = engine_rate() as f32;
        let owed: Vec<(VoiceId, (usize, usize, usize))> = self
            .engine_state
            .voices
            .iter()
            .filter_map(|(&id, repr)| {
                if self.engine_state.is_pending(super::AnyId::Voice(id)) != pending {
                    return None;
                }
                let shape = self.engine_state.shifters_shape(id)?;
                (repr.shifters_built != Some(shape)).then_some((id, shape))
            })
            .collect();
        let mut cmds = Vec::with_capacity(owed.len());
        for (voice, (channels, instances, window)) in owed {
            if let Some(repr) = self.engine_state.voices.get_mut(&voice) {
                repr.shifters_built = Some((channels, instances, window));
            }
            let shifters = Box::new(VoiceShifters::new(channels, instances, rate, window));
            cmds.push((
                voice,
                Command::SetShifters(SetShiftersArgs { voice, shifters }),
            ));
        }
        cmds
    }
}

/// What a new voice plays.
pub enum LoadSource {
    Track(TrackId),
    /// A buffer's memory (RECb).
    Buffer(Arc<BufData>),
    /// Nothing recorded: the voice plays its waves only (WAVa).
    Waves,
}

/// Arguments for `Command::Load`.
pub struct LoadArgs {
    pub voice: VoiceId,
    pub source: LoadSource,
    pub tempo: TempoUse,
    /// Made without `on`: adopts its group's tempo when grouped.
    pub adopts: bool,
}

/// Arguments for `Command::Start`.
pub struct StartArgs {
    pub target: Target,
}

/// Arguments for `Command::Pause`.
pub struct PauseArgs {
    pub target: Target,
}

/// Arguments for `Command::Resume`.
pub struct ResumeArgs {
    pub target: Target,
}

/// Arguments for `Command::Stop`.
pub struct StopArgs {
    pub target: Target,
}

/// Arguments for `Command::Unload`.
pub struct UnloadArgs {
    pub voice: VoiceId,
}

/// Arguments for `Command::SetParam`: a voice's base value for one
/// parameter (clamped by the engine).
pub struct SetParamArgs {
    pub voice: VoiceId,
    pub param: VoiceParam,
    pub value: f32,
    /// 0 = at once; otherwise the glide's length in frames (`over`).
    pub frames: u32,
}

/// What `Command::SetMode` changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceMode {
    Loop(LoopMode),
    /// 0 = slicing off.
    Slices(u16),
    /// How many instances the voice has (1..=8).
    Choir(u8),
    ChoirMode(ChoirMode),
    /// Which engine plays `pitch=`/`stretch=` (REQ-099).
    Shift(ShiftMode),
    /// How the plain head reads the source (`resample=`, REQ-143). An
    /// instance that is sounding keeps its method until it is hit again.
    Resample(ResampleMode),
    /// `bounce=`: the fade at a ping-pong turnaround, in frames; 0 = off
    /// (REQ-106, PPF).
    Bounce(u32),
}

/// The engine for `pitch=`/`stretch=` (REQ-099): the WSOLA grains (the
/// default, REQ-134) or the spectral shifter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShiftMode {
    Spectral,
    #[default]
    Grains,
}

/// How a plain playback head reads the source (REQ-143): the Elliptic-BLEP
/// resampler (the default), or 4-point Hermite interpolation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResampleMode {
    Cubic,
    Iir,
}

impl Default for ResampleMode {
    /// `iir` (REQ-143), in every build.
    fn default() -> Self {
        ResampleMode::Iir
    }
}

#[cfg(test)]
thread_local! {
    static INITIAL: std::cell::Cell<ResampleMode> =
        const { std::cell::Cell::new(ResampleMode::Iir) };
}

impl ResampleMode {
    /// What a voice is made with: the default. (In the crate's own unit
    /// tests a thread can ask for `cubic` instead, see `set_initial`: the
    /// helpers of the tests that read exact source values do.)
    pub(crate) fn initial() -> Self {
        #[cfg(test)]
        {
            INITIAL.with(std::cell::Cell::get)
        }
        #[cfg(not(test))]
        {
            ResampleMode::default()
        }
    }

    /// Test builds only: the method voices made on this thread start with.
    #[cfg(test)]
    pub(crate) fn set_initial(m: ResampleMode) {
        INITIAL.with(|c| c.set(m));
    }
}

/// Arguments for `Command::SetShifters` (REQ-099): a voice's shifters, built
/// on the command thread. The engine returns the old ones as garbage.
pub struct SetShiftersArgs {
    pub voice: VoiceId,
    pub shifters: Box<crate::audio_processing::engine::shift::VoiceShifters>,
}

/// Arguments for `Command::SetMode`.
pub struct SetModeArgs {
    pub voice: VoiceId,
    pub mode: VoiceMode,
}

/// Arguments for `Command::SetTrack`: the voice plays another track.
pub struct SetTrackArgs {
    pub voice: VoiceId,
    pub track: TrackId,
}

/// Every parameter at its default.
fn default_params() -> [f32; VoiceParam::COUNT] {
    VoiceParam::ALL.map(VoiceParam::default_value)
}

impl CmdProcessor {
    /// Creates a voice called `name` on `track` (the language's `voice`):
    /// allocates its ID and tempo and records it in the mirror. A voice that
    /// follows its parent (`TempoLink::Parent`) adopts its group's tempo when
    /// grouped.
    pub(crate) fn load_voice(
        &mut self,
        name: &str,
        source: VoiceSrc,
        choice: TempoChoice,
        link: TempoLink,
        period: u32,
    ) -> CmdResult<Command> {
        let load = match source {
            VoiceSrc::Track(t) => LoadSource::Track(t),
            VoiceSrc::Waves => LoadSource::Waves,
            VoiceSrc::Buffer(b) => match self.engine_state.buffer_by_id(b) {
                Some(repr) => LoadSource::Buffer(Arc::clone(&repr.data)),
                None => {
                    return Err(CmdErr::NoItem {
                        ty: "buffer".to_string(),
                        name: b.0.to_string(),
                    });
                }
            },
            VoiceSrc::Gone => {
                return Err(CmdErr::NoItem {
                    ty: "buffer".to_string(),
                    name: "?".to_string(),
                });
            }
        };
        // everything is valid; allocate, undoing on failure
        let voice = alloc(&mut self.engine_state.ids.voices, "voices")?;
        let tempo = match self.alloc_tempo(choice) {
            Ok(tempo) => tempo,
            Err(e) => {
                self.engine_state.ids.voices.free(voice);
                return Err(e);
            }
        };

        self.insert_tempo(tempo, TempoOwner::Voice(voice), ClockRun::Stopped);
        self.engine_state.voices.insert(
            voice,
            VoiceRepr {
                id: voice,
                name: name.to_string(),
                source,
                group: None,
                tempo: tempo.id(),
                link,
                period,
                params: default_params(),
                loop_mode: LoopMode::Off,
                slices: 0,
                choir: 1,
                choir_mode: ChoirMode::Sequential,
                shift: ShiftMode::default(),
                resample: ResampleMode::initial(),
                window: default_window(engine_rate() as f32),
                bounce: default_bounce(engine_rate() as f32),
                shifters_built: None,
                procs: BTreeMap::new(),
                fx: Vec::new(),
                fx_cap: 0,
                waves: Vec::new(),
                active: false,
            },
        );
        self.engine_state
            .voice_names
            .insert(name.to_string(), voice);

        Ok(Command::Load(LoadArgs {
            voice,
            source: load,
            tempo,
            adopts: link.follows_parent(),
        }))
    }

    /// The command for a transport action on `targets`: one command per
    /// target, a Batch for several. Words that aren't transport give an
    /// empty Batch. The mirror's run states follow the engine's.
    pub(crate) fn transport(&mut self, word: ActionWord, targets: &[Target]) -> Command {
        for &t in targets {
            self.engine_state.run_transport(word, t);
        }
        let one = |target: Target| match word {
            ActionWord::Start => Command::Start(StartArgs { target }),
            ActionWord::Stop => Command::Stop(StopArgs { target }),
            ActionWord::Pause => Command::Pause(PauseArgs { target }),
            ActionWord::Resume => Command::Resume(ResumeArgs { target }),
            _ => Command::Batch(BatchArgs { cmds: Vec::new() }),
        };
        match targets {
            [target] => one(*target),
            _ => Command::Batch(BatchArgs {
                cmds: targets.iter().map(|&t| one(t)).collect(),
            }),
        }
    }

    /// Sets a voice's base value (the glide's target, for `frames > 0`) in the
    /// mirror and returns the command.
    pub(crate) fn set_voice_param(
        &mut self,
        voice: VoiceId,
        param: VoiceParam,
        value: f32,
        frames: u32,
    ) -> Command {
        if let Some(repr) = self.engine_state.voices.get_mut(&voice) {
            repr.params[param.index()] = value;
        }
        Command::SetParam(SetParamArgs {
            voice,
            param,
            value,
            frames,
        })
    }

    /// Sets a voice's loop mode or slice count in the mirror and
    /// returns the command.
    pub(crate) fn set_voice_mode(&mut self, voice: VoiceId, mode: VoiceMode) -> Command {
        if let Some(repr) = self.engine_state.voices.get_mut(&voice) {
            match mode {
                VoiceMode::Loop(m) => repr.loop_mode = m,
                VoiceMode::Slices(n) => repr.slices = n,
                VoiceMode::Choir(n) => repr.choir = n,
                VoiceMode::ChoirMode(m) => repr.choir_mode = m,
                VoiceMode::Shift(m) => repr.shift = m,
                VoiceMode::Resample(m) => repr.resample = m,
                VoiceMode::Bounce(n) => repr.bounce = n,
            }
        }
        Command::SetMode(SetModeArgs { voice, mode })
    }

    /// Points a voice at another track in the mirror and returns the command.
    pub(crate) fn set_voice_track(&mut self, voice: VoiceId, track: TrackId) -> Command {
        if let Some(repr) = self.engine_state.voices.get_mut(&voice) {
            repr.source = VoiceSrc::Track(track);
        }
        Command::SetTrack(SetTrackArgs { voice, track })
    }

    /// Points a voice at a buffer in the mirror and returns the command.
    pub(crate) fn set_voice_buffer(&mut self, voice: VoiceId, buffer: BufferId) -> Option<Command> {
        let data = Arc::clone(&self.engine_state.buffer_by_id(buffer)?.data);
        if let Some(repr) = self.engine_state.voices.get_mut(&voice) {
            repr.source = VoiceSrc::Buffer(buffer);
        }
        Some(Command::SetSource(SetSourceArgs {
            voice,
            source: VoiceSource::Buffer(data),
        }))
    }
}

#[cfg(test)]
mod tests {
    use crate::audio_processing::blast_time::blast_time::TempoUnit;
    use crate::audio_processing::commands::lang::Code;
    use crate::audio_processing::commands::testing::*;
    use crate::audio_processing::commands::*;
    use crate::audio_processing::engine::VoiceParam;
    use crate::audio_processing::engine::arena::Limits;

    #[test]
    fn load_assigns_ids() {
        let mut cp = booted(&["kick", "snare", "pad"]);
        // without `on` a voice follows main (tempo 0) and adopts a group's
        let args = load_args(ok(&mut cp, "voice kick from \"kick\""));
        assert_eq!(args.voice, VoiceId(0));
        assert!(args.adopts);
        assert_eq!(args.tempo, TempoUse::Shared(TempoId(0)));
        assert_eq!(cp.engine_state.voices[&VoiceId(0)].link, TempoLink::Parent);

        let args = load_args(ok(&mut cp, "voice snare from \"snare\" on 120b"));
        assert_eq!(args.voice, VoiceId(1));
        assert!(!args.adopts);
        assert_eq!(
            args.tempo,
            TempoUse::Own(TempoInit {
                id: TempoId(1),
                unit: TempoUnit::Bpm,
                interval: 120.0
            })
        );
        assert_eq!(
            cp.engine_state.tempos[&TempoId(1)].owner,
            TempoOwner::Voice(VoiceId(1))
        );

        // a one-second clock of its own, as the old default was
        let args = load_args(ok(&mut cp, "voice pad from \"pad\" on 1000ms"));
        assert_eq!(
            args.tempo,
            TempoUse::Own(TempoInit {
                id: TempoId(2),
                unit: TempoUnit::Millis,
                interval: 1000.0
            })
        );

        assert_eq!(vid(&cp, "kick"), VoiceId(0));
        assert_eq!(vid(&cp, "snare"), VoiceId(1));
    }

    #[test]
    fn load_errors() {
        let mut cp = processor(&["kick"]);
        let code = |cp: &mut CmdProcessor, line: &str| lerr(cp, line)[0].code;
        assert_eq!(code(&mut cp, "voice kick from \"nope\""), Code::UnknownName);
        assert_eq!(
            code(&mut cp, "voice kick from \"kick\" on nope"),
            Code::UnknownName
        );
        assert_eq!(
            code(&mut cp, "voice kick from \"kick\" on x1"),
            Code::UnknownName
        );
        assert!(cp.engine_state.voices.is_empty());
        ok(&mut cp, "voice kick from \"kick\" on 1000ms");
        // stating it again changes nothing
        assert!(local(&mut cp, "voice kick from \"kick\" on 1000ms").contains("W101"));
        assert_eq!(cp.engine_state.voices.len(), 1);
    }

    #[test]
    fn unload_keeps_other_ids() {
        let mut cp = loaded(&["a", "b", "c"]);
        match ok(&mut cp, "remove a") {
            Command::Unload(args) => assert_eq!(args.voice, VoiceId(0)),
            _ => panic!("not an Unload"),
        }
        assert!(!cp.engine_state.voice_names.contains_key("a"));
        assert_eq!(vid(&cp, "b"), VoiceId(1));
        assert_eq!(vid(&cp, "c"), VoiceId(2));
        assert_eq!(lerr(&mut cp, "remove a")[0].code, Code::UnknownName);

        // a's ID is reused
        ok(&mut cp, "voice a from \"a\" on 1000ms");
        assert_eq!(vid(&cp, "a"), VoiceId(0));
    }

    #[test]
    fn rate_targets_group_voice() {
        let mut cp = grouped(&["a", "b", "c"], "group g on 240b { a; b }");
        match ok(&mut cp, "g.b rate=2") {
            Command::SetParam(args) => {
                assert_eq!(args.voice, VoiceId(1));
                assert_eq!(args.voice, vid(&cp, "g.b"));
                assert_eq!(args.param, VoiceParam::Rate);
                assert_eq!(args.value, 2.0);
            }
            _ => panic!("not a SetParam"),
        }
        match ok(&mut cp, "c rate=-1.5") {
            Command::SetParam(args) => {
                assert_eq!(args.voice, VoiceId(2));
                assert_eq!(args.value, -1.5);
            }
            _ => panic!("not a SetParam"),
        }
        assert_eq!(lerr(&mut cp, "c rate=x")[0].code, Code::WrongType);
        assert_eq!(lerr(&mut cp, "c rate=99")[0].code, Code::OutOfRange);
    }

    #[test]
    fn start_targets_group_voice() {
        let mut cp = grouped(&["a", "b"], "group g on 240b { a; b }");
        for (cmd, want) in [
            ("start g.a", Target::Voice(VoiceId(0))),
            ("pause g.b", Target::Voice(VoiceId(1))),
            ("start g", Target::Group(GroupId(0))),
        ] {
            let target = match ok(&mut cp, cmd) {
                Command::Start(args) => args.target,
                Command::Pause(args) => args.target,
                _ => panic!("unexpected Command"),
            };
            assert_eq!(target, want, "{}", cmd);
        }
    }

    #[test]
    fn missing_group_voice_errors() {
        let mut cp = grouped(&["a"], "group g on 240b { a }");
        let e = lerr(&mut cp, "start g.zzz");
        assert_eq!(e[0].code, Code::UnknownName);
        assert!(e[0].message.contains("zzz") && e[0].message.contains("'g'"));
        assert_eq!(lerr(&mut cp, "start nope.a")[0].code, Code::UnknownName);
        assert_eq!(lerr(&mut cp, "start g.a.b")[0].code, Code::UnknownName);
        // a grouped voice keeps its own name too (G1)
        assert!(matches!(
            ok(&mut cp, "start a"),
            Command::Start(StartArgs {
                target: Target::Voice(VoiceId(0))
            })
        ));
    }

    #[test]
    fn display_names() {
        let cp = grouped(&["a", "b"], "group g on 240b { b }");
        assert_eq!(
            cp.engine_state.display_name(vid(&cp, "a")),
            Some("a".into())
        );
        assert_eq!(
            cp.engine_state.display_name(vid(&cp, "g.b")),
            Some("g.b".into())
        );
        assert_eq!(cp.engine_state.display_name(VoiceId(9)), None);
    }

    #[test]
    fn ids_are_reused_after_unload() {
        use crate::audio_processing::test_util::{run, setup, track};
        let (mut cp, mut c) = setup(
            1,
            vec![track("a", 1, 16), track("b", 2, 16), track("c", 3, 16)],
        );
        run(&mut cp, &mut c, "voice a from \"a\" on 1000ms");
        run(&mut cp, &mut c, "voice b from \"b\" on 1000ms");
        let a = vid(&cp, "a");
        run(&mut cp, &mut c, "remove a");
        run(&mut cp, &mut c, "voice c from \"c\" on 1000ms");
        assert_eq!(vid(&cp, "c"), a);
        assert_eq!(vid(&cp, "b"), VoiceId(1));
        // the engine has c (track value 3) in a's old slot
        let v = c.voices.get(a.index()).unwrap();
        assert_eq!(crate::audio_processing::test_util::to_i16(v.track()[0]), 3);
    }

    // the pools' live counts and the maps' sizes
    fn snapshot(cp: &CmdProcessor) -> [usize; 9] {
        let es = &cp.engine_state;
        [
            es.ids.voices.live() as usize,
            es.ids.groups.live() as usize,
            es.ids.tempos.live() as usize,
            es.ids.procs.live() as usize,
            es.voices.len(),
            es.groups.len(),
            es.tempos.len(),
            es.procs.len(),
            es.voice_names.len(),
        ]
    }

    #[test]
    fn limit_reached_leaves_mirror_unchanged() {
        let limited = |limits: Limits, setup: &[&str], cmd: &str, kind: &str| {
            let mut cp = processor_with(&["a", "b", "c"], limits);
            for s in setup {
                ok(&mut cp, s);
            }
            let before = snapshot(&cp);
            let errs = lerr(&mut cp, cmd);
            assert_eq!(errs[0].code, Code::LimitReached, "{cmd}");
            assert!(errs[0].message.contains(kind), "{cmd}: {}", errs[0]);
            assert_eq!(snapshot(&cp), before, "{cmd}");
        };
        let d = Limits::default();
        let voice = |n: &str| format!("voice {n} from \"{n}\" on 1000ms");

        limited(
            Limits { voices: 2, ..d },
            &[&voice("a"), &voice("b")],
            &voice("c"),
            "voices",
        );
        limited(
            Limits { tempos: 1, ..d },
            &[&voice("a")],
            "voice b from \"b\" on 90b",
            "tempos",
        );
        // the voice's ID was allocated first and is given back
        limited(
            Limits { tempos: 1, ..d },
            &[&voice("a")],
            &voice("b"),
            "tempos",
        );
        limited(
            Limits { procs: 1, ..d },
            &[&voice("a"), "a { seq s 1 }"],
            "a { seq t 2 }",
            "processes",
        );
        // the process's ID is given back when its tempo can't be had
        limited(
            Limits { tempos: 1, ..d },
            &[&voice("a")],
            "a { seq s 1 on 120b }",
            "tempos",
        );
        limited(
            Limits {
                group_members: 2,
                ..d
            },
            &[&voice("a"), &voice("b"), &voice("c")],
            "group g on 240b { a; b; c }",
            "group members",
        );
        limited(
            Limits { groups: 1, ..d },
            &[&voice("a"), &voice("b"), "group g on 240b { a }"],
            "group h on 240b { b }",
            "groups",
        );
        // the group's ID is given back when its tempo can't be had
        limited(
            Limits { tempos: 2, ..d },
            &[&voice("a"), &voice("b")],
            "group g on 240b { a }",
            "tempos",
        );
        limited(Limits { tempos: 0, ..d }, &[], "clock x 1b", "tempos");

        let e = CmdErr::LimitReached {
            kind: "voices",
            limit: 2,
        };
        assert_eq!(e.to_string(), "Too many voices (limit 2)");
    }

    #[test]
    fn failed_commands_free_nothing_and_allocate_nothing() {
        let names = ["a", "b", "c"];
        for cmd in [
            "group g on 240b { a; nope }",
            "group g on 240b { nope; b }",
            "group g on x1 { a }",
            "group g on nope { a }",
            "group g on 240b { c; nope }",
            "voice z from \"nope\"",
            "voice z from \"a\" on x1",
            "voice z from \"a\" on nope",
            // a seq needs steps
            "a { seq z on 1b }",
            "remove nope",
        ] {
            let mut cp = loaded(&names);
            ok(&mut cp, "a { seq beat 1 }");
            let before = snapshot(&cp);
            err(&mut cp, cmd);
            assert_eq!(snapshot(&cp), before, "{cmd}");
        }
    }
}
