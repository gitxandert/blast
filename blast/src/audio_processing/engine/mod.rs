//! The audio engine (README §3.4).

use std::collections::HashMap;

use crate::audio_processing::{
    blast_time::blast_time::TempoState,
    commands::*, // too many to list
};
use crate::file_parsing::decode_helpers::AudioFile;

pub mod arena;
pub(crate) mod blep;
pub mod buffer;
pub mod events;
pub mod fx;
mod grains;
mod group;
mod handlers;
pub(crate) mod param;
mod process;
mod render;
mod schedule;
pub mod shift;
mod tempo;
pub mod track;
mod voice;
pub mod wave;

use arena::{Arena, Limits};
pub use track::Track;

pub use events::{EngineRx, Event, Garbage};

pub use blep::BlepTable;
pub use buffer::{BufData, BufSource, BufferSlot, NoMemory, SaveBox, SavedTake};
pub use fx::{FX_PARAMS, FxChain, FxKind, FxSlot, FxType};
pub use group::{Group, GroupState};
pub use process::ProcSlot;
pub use schedule::Schedule;
pub use tempo::Tempo;
pub(crate) use voice::{BOUNCE_RANGE_S, default_bounce};
pub use voice::{
    ChoirMode, LoopMode, MAX_CHOIR, MAX_SLICES, MAX_VOICE_CHANNELS, ModOp, ModSlot, ModTarget,
    Voice, VoiceParam, VoiceSource, VoiceState,
};
pub use wave::{
    MAX_TABLE_FRAMES, MAX_TABLE_SLICES, MAX_WAVES, MIN_TABLE_FRAMES, WAVE_PARAMS, WaveParam,
    WaveShape, WaveTable, Waves, fits, frames_ok, levels_for, wave_defaults,
};

/// The audio engine: owns every Voice, Group, tempo, Process and track, in
/// arenas indexed by the mirror's IDs, and mixes them.
pub struct Conductor {
    pub(crate) voices: Arena<Voice>,
    pub(crate) groups: Arena<Group>,
    pub(crate) tempos: Arena<Tempo>,
    pub(crate) procs: Arena<ProcSlot>,
    pub(crate) tracks: Arena<Track>,
    // buffers by slot; only a command that carries a bigger table (`room`)
    // grows it
    pub(crate) buffers: Vec<Option<BufferSlot>>,
    // the buffers whose source is `out`, by slot; replaced by `SetTaps`
    pub(crate) out_taps: Vec<u32>,
    // lines waiting for their beat (SCHED); preallocated
    pub(crate) schedule: Schedule,
    // frames rendered so far: the clock `then` and `thenif` count on
    pub(crate) frame: u64,
    // the longest glide, in frames, started since the last `MarkGlides`
    pub(crate) glide_mark: u32,
    // the frame the last glide started so far is over (export's end)
    pub(crate) glide_end: u64,
    // the frame a waiting entry last retired on (export's end)
    pub(crate) last_retired: u64,
    // while a timed entry applies: the clock it counted on and the position
    // it was due at (read by `SetMark`)
    pub(crate) firing: Option<(TempoId, f64)>,
    out_channels: usize,
    #[allow(dead_code)] // kept for INS, which reports the limits
    limits: Limits,
    events: rtrb::Producer<Event>,
    garbage: rtrb::Producer<Garbage>,
    // garbage that didn't fit in the queue; capacity `limits.parked`,
    // never grown
    parked: Vec<Garbage>,
    // one frame per group slot (`limits.groups * out_channels`): a group's
    // members mix into its slice, the group's chain runs on it, and its
    // gain scales it; never grown
    bus: Vec<f32>,
    // the same for a voice with effects: its chain runs on this frame
    fx_scratch: Vec<f32>,
    // what the waves' band-limiting needs at the session's sample rate,
    // built here, never on the audio thread
    blep: Box<BlepTable>,
    // objects leaked since the last render's end, reported then
    leaked: u32,
    // the REPL ends of the queues, kept by `prepare` until `take_rx`
    unclaimed: Option<EngineRx>,
}

// the Conductor moves to the audio thread, the mirror to the REPL thread;
// commands and garbage cross between them
const _: () = {
    fn assert_send<T: Send>() {}
    fn assert_send_copy<T: Send + Copy>() {}
    fn assert_send_clone<T: Send + Clone>() {}
    #[allow(dead_code)]
    fn check() {
        assert_send::<Conductor>();
        assert_send::<Command>();
        assert_send::<Garbage>();
        assert_send::<EngineRx>();
        assert_send_copy::<Event>();
        assert_send_clone::<EngineState>();
    }
};

impl Conductor {
    /// Tracks sorted by name; `TrackId(i)` is the i-th. Limits::default(),
    /// with `tracks` raised to the track count if that's larger. Keeps the
    /// REPL ends of the queues until `take_rx`.
    pub fn prepare(out_channels: usize, tracks: HashMap<String, AudioFile>) -> Self {
        let mut files: Vec<AudioFile> = tracks.into_values().collect();
        files.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        Self::keeping_rx(Limits::default(), out_channels, files)
    }

    /// `new`, keeping the REPL ends of the queues until `take_rx`.
    pub(crate) fn keeping_rx(limits: Limits, out_channels: usize, tracks: Vec<AudioFile>) -> Self {
        let (mut conductor, rx) = Self::new(limits, out_channels, tracks);
        conductor.unclaimed = Some(rx);
        conductor
    }

    /// Every arena and queue allocated at its limit now; `tracks` sorted by
    /// name, the track limit raised to their count (as
    /// `EngineState::with_limits` does). Returns the REPL ends of the queues.
    pub fn new(limits: Limits, out_channels: usize, tracks: Vec<AudioFile>) -> (Self, EngineRx) {
        let mut files = tracks;
        files.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        let tracks = files.into_iter().map(Track::from_file).collect();
        Self::with_tracks(limits, out_channels, tracks)
    }

    /// `prepare` for tracks the mirror already holds (`EngineState::
    /// shared_tracks`): the engine and the mirror share each track's
    /// samples, so a startup asset lives once. Keeps the REPL ends.
    pub fn prepare_shared(out_channels: usize, tracks: Vec<Track>) -> Self {
        let (mut conductor, rx) = Self::with_tracks(Limits::default(), out_channels, tracks);
        conductor.unclaimed = Some(rx);
        conductor
    }

    /// `new` for tracks already built, in `TrackId` order.
    pub fn with_tracks(
        limits: Limits,
        out_channels: usize,
        tracks: Vec<Track>,
    ) -> (Self, EngineRx) {
        let mut limits = limits;
        limits.tracks = limits.tracks.max(tracks.len() as u32);

        let mut track_arena = Arena::with_capacity(limits.tracks as usize);
        for (i, track) in tracks.into_iter().enumerate() {
            // capacity is at least the track count, so every slot is free
            let _ = track_arena.insert(i, track);
        }

        let (events, events_rx) = rtrb::RingBuffer::new(limits.events);
        let (garbage, garbage_rx) = rtrb::RingBuffer::new(limits.garbage);
        let conductor = Self {
            voices: Arena::with_capacity(limits.voices as usize),
            groups: Arena::with_capacity(limits.groups as usize),
            tempos: Arena::with_capacity(limits.tempos as usize),
            procs: Arena::with_capacity(limits.procs as usize),
            tracks: track_arena,
            buffers: Vec::new(),
            out_taps: Vec::new(),
            schedule: Schedule::with_capacity(limits.scheduled as usize, limits.scripts as usize),
            frame: 0,
            glide_mark: 0,
            glide_end: 0,
            last_retired: 0,
            firing: None,
            out_channels,
            limits,
            events,
            garbage,
            parked: Vec::with_capacity(limits.parked),
            bus: vec![0.0; limits.groups as usize * out_channels],
            fx_scratch: vec![0.0; out_channels],
            blep: BlepTable::new(
                match crate::audio_processing::blast_time::sample_rate::get() {
                    0 => 48_000.0,
                    n => f64::from(n),
                },
            ),
            leaked: 0,
            unclaimed: None,
        };
        let rx = EngineRx {
            events: events_rx,
            garbage: garbage_rx,
        };
        (conductor, rx)
    }

    /// The queue ends `prepare` kept (once; then `None`).
    pub fn take_rx(&mut self) -> Option<EngineRx> {
        self.unclaimed.take()
    }

    /// Output channels the Conductor renders.
    pub fn out_channels(&self) -> usize {
        self.out_channels
    }

    /// Queues an event; drops it if the queue is full. Never blocks.
    pub(crate) fn emit(&mut self, event: Event) {
        // a full queue means the REPL isn't reading; losing a notice is fine
        let _ = self.events.push(event);
    }

    /// Pushes `g` to the garbage queue, or hands it back if the queue is full.
    #[allow(clippy::result_large_err)] // handing it back is the point
    pub(crate) fn discard(&mut self, g: Garbage) -> Result<(), Garbage> {
        self.garbage.push(g).map_err(|rtrb::PushError::Full(g)| g)
    }

    /// `discard`, else park it (retried at the start of each `render`), else
    /// leak it with `mem::forget` and emit `GarbageLeaked`. Never frees.
    pub(crate) fn retire(&mut self, g: Garbage) {
        retire_into(&mut self.garbage, &mut self.parked, &mut self.leaked, g);
    }

    // moves parked garbage into the queue while it has room
    fn flush_parked(&mut self) {
        while let Some(g) = self.parked.pop() {
            if let Err(g) = self.discard(g) {
                // room was checked by the pop, so this never reallocates
                self.parked.push(g);
                break;
            }
        }
    }

    // reports the objects leaked since the last report
    fn report_leaks(&mut self) {
        if self.leaked > 0 {
            self.emit(Event::GarbageLeaked { count: self.leaked });
            self.leaked = 0;
        }
    }

    /// The frame by which every glide started so far is over (the longest
    /// one is counted whole, even if a later setting cut it short). Export
    /// renders up to it after the script ends.
    pub fn glides_done_at(&self) -> u64 {
        self.glide_end
    }

    /// The frame a waiting entry last retired on (it fired or was dropped);
    /// 0 if none has. Export's script ends there.
    pub fn last_retired_at(&self) -> u64 {
        self.last_retired
    }

    /// Frames rendered so far.
    pub fn frames_rendered(&self) -> u64 {
        self.frame
    }

    /// Applies every command waiting in `commands` and says how many there
    /// were; runs on the audio thread.
    pub fn apply_pending(&mut self, commands: &mut CmdReceiver) -> usize {
        let mut n = 0;
        while let Some(cmd) = commands.try_pop() {
            self.apply(cmd);
            n += 1;
        }
        n
    }

    /// One block of the audio callback: applies what is queued, then
    /// renders `out` (interleaved, unclamped). The device callback and
    /// `--export` both go through here, so they can't drift apart.
    pub fn run_block(&mut self, commands: &mut CmdReceiver, out: &mut [f32], channels: usize) {
        self.apply_pending(commands);
        if channels == 0 {
            return;
        }
        self.render(out, channels);
    }

    /// Hands `cmd` to its handler; runs on the audio thread.
    pub fn apply(&mut self, cmd: Command) {
        // a command that removes something strands the waiting lines that
        // name it; a Batch's own commands sweep as they are applied, and an
        // entry that arrives sweeps when it fires
        let sweeps =
            creates_or_removes(&cmd) && !matches!(cmd, Command::Batch(_) | Command::Schedule(_));
        match cmd {
            Command::Load(args) => self.load(args),
            Command::Start(args) => self.start(args),
            Command::Pause(args) => self.pause(args),
            Command::Resume(args) => self.resume(args),
            Command::Stop(args) => self.stop(args),
            Command::Unload(args) => self.unload(args),
            Command::SetParam(args) => self.set_param(args),
            Command::SetMode(args) => self.set_mode(args),
            Command::SetTrack(args) => self.set_track(args),
            Command::SetShifters(args) => self.set_shifters(args),
            Command::AddTrack(args) => self.add_track(args),
            Command::DropTrack(args) => self.drop_track(args),
            Command::Group(args) => self.group(args),
            Command::GroupEdit(args) => self.group_edit(args),
            Command::GroupGain(args) => self.group_gain(args),
            Command::Tc(args) => self.tempo_context(args),
            Command::TempoSet(args) => self.retime_tempo(args),
            Command::TempoFree(args) => self.free_tempo(args),
            Command::TempoAssign(args) => self.assign_tempo(args),
            Command::Seq(args) => self.seq(args),
            Command::SeqEdit(args) => self.seq_edit(args),
            Command::SeqRecv(args) => self.seq_recv(args),
            Command::Lfo(args) => self.lfo(args),
            Command::LfoEdit(args) => self.lfo_edit(args),
            Command::Env(args) => self.env(args),
            Command::EnvEdit(args) => self.env_edit(args),
            Command::ProcEdit(args) => self.proc_edit(args),
            Command::Fx(args) => self.fx(args),
            Command::Wave(args) => self.wave(args),
            Command::Buffer(args) => self.buffer(args),
            Command::BufferEdit(args) => self.buffer_edit(args),
            Command::SetTaps(args) => self.set_taps(args),
            Command::SetSource(args) => self.set_source(args),
            Command::Schedule(args) => self.schedule_command(args),
            Command::MarkGlides(_) => self.glide_mark = 0,
            Command::SetMark(args) => self.set_mark(args),
            Command::CancelScript(args) => self.cancel_script(args),
            Command::Batch(mut args) => {
                // nested batches recurse; the mirror never builds them
                for cmd in args.cmds.drain(..) {
                    self.apply(cmd);
                }
                // the emptied Vec still owns its buffer
                self.retire(Garbage::Commands(args.cmds));
            }
            Command::Quit(_) => self.emit(Event::Quit),
        }
        if sweeps {
            self.sweep_waiting();
        }
    }

    /// Whether a voice is playing: active, and its group too if it has one.
    #[allow(dead_code)] // the render loop uses the free function; INS reads it
    pub(crate) fn voice_running(&self, v: VoiceId) -> bool {
        voice_running(&self.voices, &self.groups, v)
    }

    /// Whether a group is playing.
    #[allow(dead_code)] // the render loop uses the free function; INS reads it
    pub(crate) fn group_running(&self, g: GroupId) -> bool {
        group_running(&self.groups, g)
    }

    /// Notes a glide of `frames` that was just started: `then` waits for the
    /// longest one since the last `MarkGlides`.
    pub(crate) fn note_glide(&mut self, frames: u32) {
        self.glide_mark = self.glide_mark.max(frames);
        self.glide_end = self.glide_end.max(self.frame + u64::from(frames));
    }

    /// Removes a voice with its processes and owned tempos (REQ-011 item 15).
    pub(crate) fn remove_voice(&mut self, voice: VoiceId) {
        let Some(removed) = self.voices.remove(voice.index()) else {
            debug_assert!(false, "unload of a missing voice {voice:?}");
            return;
        };

        if let Some(group) = removed.group.and_then(|g| self.groups.get_mut(g.index()))
            && let Some(pos) = group.members.iter().position(|m| *m == voice)
        {
            // keeps member order; never allocates
            group.members.remove(pos);
        }
        // the group's envs watch the sum over its members, which just moved
        if let Some(g) = removed.group {
            self.resync_group_envs(g);
        }

        // a Seq owns its step Vecs, so processes leave as garbage
        let (procs, parked, garbage, leaked) = (
            &mut self.procs,
            &mut self.parked,
            &mut self.garbage,
            &mut self.leaked,
        );
        procs.remove_where(
            |_, p| p.owner == ProcOwner::Voice(voice),
            |_, slot| retire_into(garbage, parked, leaked, Garbage::Process(slot.process)),
        );
        // tempos are plain data, dropped in place
        self.tempos
            .remove_where(|_, t| t.owner == TempoOwner::Voice(voice), |_, _| ());
        // the Voice holds an Arc clone of its track; the last clone may be
        // this one if the track arena ever lets go, so it leaves as garbage
        self.retire(Garbage::Voice(removed));
    }

    // creates a tempo in its slot
    fn add_tempo(&mut self, init: TempoInit, owner: TempoOwner) {
        let tempo = Tempo {
            state: TempoState::new(init.unit, init.interval),
            owner,
            glide_gen: 0,
        };
        if self.tempos.insert(init.id.index(), tempo).is_err() {
            debug_assert!(false, "tempo slot {:?} taken", init.id);
        }
    }

    // the tempo an object uses, if its slot is filled
    fn tempo_state(&self, id: TempoId) -> Option<&TempoState> {
        self.tempos.get(id.index()).map(|t| &t.state)
    }
}

/// `Conductor::retire` over the fields it needs, for callers that hold
/// another arena borrowed (as `remove_where` does).
fn retire_into(
    garbage: &mut rtrb::Producer<Garbage>,
    parked: &mut Vec<Garbage>,
    leaked: &mut u32,
    g: Garbage,
) {
    let Err(rtrb::PushError::Full(g)) = garbage.push(g) else {
        return;
    };
    // the check keeps the push from reallocating
    if parked.len() < parked.capacity() {
        parked.push(g);
    } else {
        // freeing it here would free on the audio thread; a leak is the
        // documented outcome (README §3.4, REQ-029)
        std::mem::forget(g);
        *leaked = leaked.saturating_add(1);
    }
}

/// Whether a voice is playing: active, and its group too if it has one.
/// A free function over the arenas, so the render loop can call it while it
/// borrows other arenas mutably.
pub(crate) fn voice_running(voices: &Arena<Voice>, groups: &Arena<Group>, v: VoiceId) -> bool {
    voices
        .get(v.index())
        .is_some_and(|voice| voice_is_running(voice, groups))
}

pub(crate) fn voice_is_running(voice: &Voice, groups: &Arena<Group>) -> bool {
    voice.state.active
        && match voice.group {
            Some(g) => group_running(groups, g),
            None => true,
        }
}

/// Whether a group is playing.
pub(crate) fn group_running(groups: &Arena<Group>, g: GroupId) -> bool {
    groups.get(g.index()).is_some_and(|g| g.state.active)
}

#[cfg(test)]
mod buffer_tests;
#[cfg(test)]
mod group_tests;
#[cfg(test)]
mod tests;
