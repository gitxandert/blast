//! Buffers (RECb): the arguments of the buffer commands, the mirror of a
//! buffer, and the typed core the language lowers into. The mirror decides
//! slots, sources and states, and builds all memory here, on the command
//! thread; the engine installs what arrives (REQ-095).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::lang::Code;
use super::reprs::{BufferId, EngineState, GroupId, TempoId, VoiceId};
use super::track::engine_rate;
use super::voice::VoiceSrc;
use super::wave::WaveFrom;
use super::{CmdProcessor, CmdSender, Command};
use crate::audio_processing::blast_rand::fast_seed;
use crate::audio_processing::engine::{
    BufData, BufSource, BufferSlot, EngineRx, NoMemory, SaveBox, VoiceSource,
};
use crate::export::{RecHandle, RecSpec, SampleKind, WriterOpts};
use crate::file_parsing::decode_helpers::AudioFile;

/// Arguments for `Command::Buffer`: a buffer takes `slot`, replacing the
/// memory of one that is there. Idle until a `BufOp::Start`.
pub struct BufferArgs {
    pub slot: u32,
    pub data: Arc<BufData>,
    pub source: BufSource,
    pub looping: bool,
    /// 8, 16, 24 or 32.
    pub bits: u8,
    /// Seeds the dither.
    pub seed: u64,
    /// A bigger empty table (every cell `None`), sent when the slot doesn't
    /// fit the engine's: the engine moves its buffers into it.
    pub room: Option<Vec<Option<BufferSlot>>>,
}

/// What `Command::BufferEdit` does to a buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufOp {
    Start,
    Pause,
    Resume,
    Stop,
    Looping(bool),
    Source(BufSource),
    Remove,
}

/// Arguments for `Command::BufferEdit`.
pub struct BufferEditArgs {
    pub slot: u32,
    pub op: BufOp,
    /// A `Start` of a buffer with `to=` carries the take's private copy
    /// (REQ-095 point 5).
    pub save: Option<SaveBox>,
    /// A `Start`'s take number, which the engine echoes in `BufferEnded` so
    /// the mirror can tell this take's end from an earlier one's.
    pub take: u32,
}

impl BufferEditArgs {
    pub fn new(slot: u32, op: BufOp) -> Self {
        Self {
            slot,
            op,
            save: None,
            take: 0,
        }
    }
}

/// Whose tap list a command replaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapOwner {
    Voice(VoiceId),
    Group(GroupId),
    /// The output mix: the buffers whose source is `out`.
    Out,
}

/// Arguments for `Command::SetTaps`: the buffers (slots) that record
/// `owner`. The list is built on the command thread; the old one leaves as
/// garbage.
pub struct SetTapsArgs {
    pub owner: TapOwner,
    pub taps: Vec<u32>,
}

/// Arguments for `Command::SetSource`: the voice plays this memory.
pub struct SetSourceArgs {
    pub voice: VoiceId,
    pub source: VoiceSource,
}

/// Where a buffer's take is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Take {
    Idle,
    Recording,
    Paused,
}

/// Where a take goes when it ends (`to=`).
#[derive(Clone, Debug, PartialEq)]
pub enum ToDest {
    /// A WAV file, overwritten.
    File(PathBuf),
    /// A track of this name, made or replaced.
    Track(String),
}

/// `to=` as it was written, and what it came to.
#[derive(Clone, Debug, PartialEq)]
pub struct BufTo {
    pub written: String,
    pub dest: ToDest,
}

impl BufTo {
    /// Classifies a written `to=`: a name with an extension or a `/` is a
    /// file (`~/` is `home`), anything else a track. `""` is no output.
    pub(crate) fn parse(written: &str, home: Option<std::ffi::OsString>) -> Option<BufTo> {
        if written.is_empty() {
            return None;
        }
        let path = Path::new(written);
        let dest = if written.contains('/') || path.extension().is_some() {
            ToDest::File(match (written.strip_prefix("~/"), home) {
                (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
                _ => path.to_path_buf(),
            })
        } else {
            ToDest::Track(written.to_string())
        };
        Some(BufTo {
            written: written.to_string(),
            dest,
        })
    }

    /// The reason the file can't be written, if its folder isn't there.
    pub(crate) fn folder_problem(&self) -> Option<String> {
        let ToDest::File(path) = &self.dest else {
            return None;
        };
        let folder = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        match std::fs::metadata(folder) {
            Ok(m) if m.is_dir() => None,
            Ok(_) => Some(format!("{} isn't a folder", folder.display())),
            Err(e) => Some(format!("{}: {e}", folder.display())),
        }
    }
}

/// A take that has `to=`, from its `start` until its copy comes back.
#[derive(Clone, Debug)]
struct PendingSave {
    buffer: String,
    slot: u32,
    to: BufTo,
    bits: u8,
    channels: u16,
}

/// A file being written for a take.
#[derive(Clone)]
struct Writer {
    buffer: String,
    // the file as written in `to=`
    shown: String,
    handle: RecHandle,
}

/// Takes with `to=` on their way to a track or a file (RECc). The engine
/// hands each take's copy back with the ticket its `start` carried.
#[derive(Clone, Default)]
pub struct BufSaves {
    next_ticket: u64,
    pending: HashMap<u64, PendingSave>,
    writers: Vec<Writer>,
    notices: Vec<String>,
}

/// A length as it was written, for `state`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LenSpec {
    /// Seconds (`30s`, `500ms`, `48000f` all come to this).
    Secs(f64),
    /// Ticks of the buffer's clock (`4b`, or a bare `4`).
    Ticks(f64),
}

/// A buffer in the mirror.
#[derive(Clone)]
pub struct BufferRepr {
    pub(crate) id: BufferId,
    pub(crate) name: String,
    pub(crate) source: BufSource,
    /// As written.
    pub(crate) length: LenSpec,
    /// Frames, as `length` came to when it was written.
    pub(crate) frames: usize,
    /// What `b` counts.
    pub(crate) clock: Option<TempoId>,
    pub(crate) looping: bool,
    pub(crate) live: bool,
    pub(crate) bits: u8,
    pub(crate) take: Take,
    /// The number of the current take (the last `start`); an end that
    /// reports another number is an earlier take's.
    pub(crate) take_no: u32,
    /// Where a take goes when it ends; read when a take starts.
    pub(crate) to: Option<BufTo>,
    /// The engine's memory, and what it was made for. A restated `length`,
    /// `live` or `bits` differs from this until the next `start` makes fresh memory.
    pub(crate) data: Arc<BufData>,
    pub(crate) made: (usize, bool, u8),
}

impl BufferRepr {
    /// Whether `length` or `live` was restated since the memory was made.
    pub(crate) fn stale(&self) -> bool {
        self.made != (self.frames, self.live, self.bits)
    }
}

/// What a new buffer is made of.
pub(crate) struct NewBuffer<'a> {
    pub name: &'a str,
    pub source: BufSource,
    pub length: LenSpec,
    pub frames: usize,
    pub clock: Option<TempoId>,
    pub looping: bool,
    pub live: bool,
    pub bits: u8,
    pub to: Option<BufTo>,
}

/// Memory for `frames` frames; E709 when it can't be had.
pub(crate) fn no_memory(name: &str, e: NoMemory) -> (Code, String) {
    (
        Code::NoMemory,
        format!(
            "not enough memory for buffer {name} ({})",
            crate::audio_processing::engine::buffer::human_bytes(e.bytes)
        ),
    )
}

impl EngineState {
    /// The buffer in `slot`.
    pub(crate) fn buffer_by_id(&self, id: BufferId) -> Option<&BufferRepr> {
        self.buffers.values().find(|b| b.id == id)
    }

    /// The slots recording `source`, in slot order.
    pub(crate) fn taps_of(&self, source: BufSource) -> Vec<u32> {
        let mut slots: Vec<u32> = self
            .buffers
            .values()
            .filter(|b| b.source == source)
            .map(|b| b.id.0)
            .collect();
        slots.sort_unstable();
        slots
    }

    /// The command that gives a voice or a group its tap list as it is now,
    /// or the engine its list of buffers taking the output mix.
    pub(crate) fn taps_command(&self, source: BufSource) -> Option<Command> {
        let owner = match source {
            BufSource::Out => TapOwner::Out,
            BufSource::Voice(v) => TapOwner::Voice(v),
            BufSource::Group(g) => TapOwner::Group(g),
        };
        Some(Command::SetTaps(SetTapsArgs {
            owner,
            taps: self.taps_of(source),
        }))
    }

    /// The voices playing the buffer.
    pub(crate) fn voices_playing(&self, id: BufferId) -> Vec<VoiceId> {
        self.voices
            .values()
            .filter(|v| v.source == VoiceSrc::Buffer(id))
            .map(|v| v.id)
            .collect()
    }

    /// The engine reported the take in `slot` over (stopped, or full).
    /// `take` is the number the engine echoes: an end from a take that was
    /// stopped and restarted in between is ignored.
    pub fn note_buffer_ended(&mut self, slot: u32, take: u32) {
        if let Some(b) = self
            .buffers
            .values_mut()
            .find(|b| b.id == BufferId(slot) && b.take_no == take)
        {
            b.take = Take::Idle;
        }
    }

    /// Buffers that recorded a voice or group that is going now record the
    /// mix instead, and their takes stop (W102). The commands are for the
    /// engine; the warning is for `settle_warnings`.
    pub(crate) fn orphan_buffers(&mut self, source: BufSource, gone: &str) -> Vec<Command> {
        let mut cmds = Vec::new();
        let mut notes = Vec::new();
        let mut moved = false;
        for b in self.buffers.values_mut().filter(|b| b.source == source) {
            if b.take != Take::Idle {
                b.take = Take::Idle;
                cmds.push(Command::BufferEdit(BufferEditArgs::new(
                    b.id.0,
                    BufOp::Stop,
                )));
            }
            b.source = BufSource::Out;
            moved = true;
            cmds.push(Command::BufferEdit(BufferEditArgs::new(
                b.id.0,
                BufOp::Source(BufSource::Out),
            )));
            notes.push(format!(
                "buffer '{}' recorded '{gone}', which is going: it records the output now",
                b.name
            ));
        }
        for text in notes {
            self.warnings.push((Code::ReceiverDropped, text));
        }
        if moved {
            cmds.extend(self.taps_command(BufSource::Out));
        }
        cmds
    }

    /// The memory for a buffer of `frames` frames.
    pub(crate) fn alloc_buffer(&self, frames: usize, live: bool) -> Result<Arc<BufData>, NoMemory> {
        let channels = u16::try_from(self.out_channels()).unwrap_or(u16::MAX);
        BufData::alloc(frames, channels, if live { 1 } else { 2 })
    }
}

impl CmdProcessor {
    /// Creates a buffer: allocates its memory and its slot, and records it
    /// in the mirror. The commands install it (with a bigger table when the
    /// engine's is full) and tell the source it is being recorded.
    pub(crate) fn make_buffer(&mut self, new: NewBuffer<'_>) -> Result<Vec<Command>, NoMemory> {
        let es = &mut self.engine_state;
        let data = es.alloc_buffer(new.frames, new.live)?;
        // never fails: the pool's limit is the largest slot
        let Some(id) = es.ids.buffers.alloc() else {
            return Err(NoMemory { bytes: 0 });
        };
        let slot = id.0 as usize;
        let room = (slot >= es.buffer_cap).then(|| {
            let cap = (slot + 1).max(es.buffer_cap * 2).max(4);
            es.buffer_cap = cap;
            (0..cap).map(|_| None).collect::<Vec<_>>()
        });
        es.buffers.insert(
            new.name.to_string(),
            BufferRepr {
                id,
                name: new.name.to_string(),
                source: new.source,
                length: new.length,
                frames: new.frames,
                clock: new.clock,
                looping: new.looping,
                live: new.live,
                bits: new.bits,
                take: Take::Idle,
                take_no: 0,
                to: new.to,
                data: Arc::clone(&data),
                made: (new.frames, new.live, new.bits),
            },
        );
        let mut cmds = vec![Command::Buffer(BufferArgs {
            slot: id.0,
            data,
            source: new.source,
            looping: new.looping,
            bits: new.bits,
            seed: fast_seed(),
            room,
        })];
        cmds.extend(es.taps_command(new.source));
        Ok(cmds)
    }

    /// Fresh memory for a buffer whose `length` or `live` was restated: the
    /// slot takes it, and every voice playing the buffer moves to it. The
    /// old memory leaves as garbage once the voices let go.
    pub(crate) fn renew_buffer(&mut self, name: &str) -> Result<Vec<Command>, NoMemory> {
        let es = &mut self.engine_state;
        let Some(repr) = es.buffers.get(name) else {
            return Ok(Vec::new());
        };
        let (id, frames, live) = (repr.id, repr.frames, repr.live);
        let (source, looping, bits) = (repr.source, repr.looping, repr.bits);
        let data = es.alloc_buffer(frames, live)?;
        let players = es.voices_playing(id);
        if let Some(repr) = es.buffers.get_mut(name) {
            repr.data = Arc::clone(&data);
            repr.made = (frames, live, bits);
            repr.take = Take::Idle;
        }
        let mut cmds = vec![Command::Buffer(BufferArgs {
            slot: id.0,
            data: Arc::clone(&data),
            source,
            looping,
            bits,
            seed: fast_seed(),
            room: None,
        })];
        for voice in players {
            cmds.push(Command::SetSource(SetSourceArgs {
                voice,
                source: VoiceSource::Buffer(Arc::clone(&data)),
            }));
        }
        Ok(cmds)
    }

    /// A transport action on a buffer, in the mirror, as a command.
    pub(crate) fn buffer_transport(&mut self, name: &str, op: BufOp) -> Option<Command> {
        let es = &mut self.engine_state;
        let b = es.buffers.get_mut(name)?;
        let mut take_no = 0;
        match op {
            BufOp::Start => {
                b.take = Take::Recording;
                // numbered across all buffers, so a slot reused after a
                // `remove` can't match an older take's end
                es.next_take = es.next_take.wrapping_add(1);
                b.take_no = es.next_take;
                take_no = es.next_take;
            }
            BufOp::Pause => b.take = Take::Paused,
            BufOp::Resume => b.take = Take::Recording,
            BufOp::Stop => b.take = Take::Idle,
            BufOp::Looping(on) => b.looping = on,
            BufOp::Source(_) | BufOp::Remove => {}
        }
        let mut args = BufferEditArgs::new(b.id.0, op);
        args.take = take_no;
        Some(Command::BufferEdit(args))
    }

    /// Changes what a buffer records, in the mirror: the commands move it
    /// from the tap list of the old owner to the new one's.
    pub(crate) fn set_buffer_source(&mut self, name: &str, source: BufSource) -> Vec<Command> {
        let es = &mut self.engine_state;
        let Some(b) = es.buffers.get_mut(name) else {
            return Vec::new();
        };
        let (old, slot) = (b.source, b.id.0);
        b.source = source;
        let mut cmds = vec![Command::BufferEdit(BufferEditArgs::new(
            slot,
            BufOp::Source(source),
        ))];
        cmds.extend(es.taps_command(old));
        cmds.extend(es.taps_command(source));
        cmds
    }

    /// Removes a buffer from the mirror. The commands drop it from its
    /// source's tap list and from the engine, and silence the voices that
    /// played it (they keep running, as on an empty track).
    pub(crate) fn remove_buffer(&mut self, name: &str) -> Vec<Command> {
        let es = &mut self.engine_state;
        let Some(id) = es.buffers.get(name).map(|b| b.id) else {
            return Vec::new();
        };
        let players = es.voices_playing(id);
        let Some(b) = es.buffers.remove(name) else {
            return Vec::new();
        };
        es.ids.buffers.free(id);
        // a table made from it keeps playing: it owns its data
        es.forget_table_source(WaveFrom::Buffer(id));
        let mut cmds = Vec::new();
        for v in players {
            if let Some(r) = es.voices.get_mut(&v) {
                r.source = VoiceSrc::Gone;
            }
            cmds.push(Command::SetSource(SetSourceArgs {
                voice: v,
                source: VoiceSource::Track(Arc::from(Vec::<f32>::new())),
            }));
        }
        cmds.push(Command::BufferEdit(BufferEditArgs::new(
            id.0,
            BufOp::Remove,
        )));
        cmds.extend(es.taps_command(b.source));
        cmds
    }
}

// the box for a take's copy; the memory may not be there
fn alloc_save(samples: usize) -> Result<Box<[f32]>, NoMemory> {
    let bytes = (samples as u128) * 4;
    let mut data: Vec<f32> = Vec::new();
    if bytes > isize::MAX as u128 || data.try_reserve_exact(samples).is_err() {
        return Err(NoMemory { bytes });
    }
    data.resize(samples, 0.0);
    Ok(data.into_boxed_slice())
}

// seconds as `2.0 s`
fn secs_text(frames: usize) -> String {
    format!("{:.1} s", frames as f64 / f64::from(engine_rate()))
}

// the file format a buffer's `bits` writes
fn sample_kind(bits: u8) -> SampleKind {
    match bits {
        24 => SampleKind::Int24,
        // 8 bits are written as 16-bit holding 8-bit values
        16 | 8 => SampleKind::Int16,
        _ => SampleKind::Float32,
    }
}

// pushes the samples into the writer's ring; the box is freed here, off the
// command thread's way
fn feed(mut ring: rtrb::Producer<f32>, data: Vec<f32>, samples: usize) {
    let mut rest = &data[..samples.min(data.len())];
    while !rest.is_empty() {
        if ring.is_abandoned() {
            return;
        }
        let n = ring.slots().min(rest.len()).min(1 << 16);
        if n == 0 || ring.push_entire_slice(&rest[..n]).is_err() {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        rest = &rest[n..];
    }
}

impl BufSaves {
    /// Whether a file is still being written.
    #[cfg(test)]
    pub(crate) fn writing(&self) -> bool {
        !self.writers.is_empty()
    }
}

impl CmdProcessor {
    /// The private copy for a take that is about to start, if the buffer has
    /// `to=`: a box of `length x channels` samples, made here (E709 when it
    /// can't be had), and a ticket that brings the take back to its
    /// destination.
    pub(crate) fn save_box(&mut self, name: &str) -> Result<Option<SaveBox>, NoMemory> {
        let es = &mut self.engine_state;
        let Some(b) = es.buffers.get(name) else {
            return Ok(None);
        };
        let Some(to) = b.to.clone() else {
            return Ok(None);
        };
        let channels = b.data.channels;
        let samples = b.data.capacity().saturating_mul(usize::from(channels));
        let data = alloc_save(samples)?;
        let ticket = es.saves.next_ticket;
        es.saves.next_ticket += 1;
        let (slot, bits) = (b.id.0, b.bits);
        // a 16 or 24-bit file is dithered once, by its writer
        let raw = matches!(to.dest, ToDest::File(_)) && matches!(bits, 16 | 24);
        es.saves.pending.insert(
            ticket,
            PendingSave {
                buffer: name.to_string(),
                slot,
                to,
                bits,
                channels,
            },
        );
        Ok(Some(SaveBox { ticket, data, raw }))
    }

    /// A take with `to=` came back: write its file (RECa's writer, fed by a
    /// short-lived thread) or make or replace its track. Returns the
    /// commands to send (a replaced track's voices switch to the new
    /// samples).
    pub fn note_buffer_save(
        &mut self,
        slot: u32,
        ticket: u64,
        data: Box<[f32]>,
        frames: u32,
    ) -> Vec<Command> {
        let Some(save) = self.engine_state.saves.pending.remove(&ticket) else {
            return Vec::new();
        };
        debug_assert_eq!(save.slot, slot);
        let ch = usize::from(save.channels.max(1));
        let frames = (frames as usize).min(data.len() / ch);
        let name = save.buffer;
        if frames == 0 {
            self.engine_state
                .saves
                .notices
                .push(format!("buf {name}: nothing was recorded"));
            return Vec::new();
        }
        let BufTo { written, dest } = save.to;
        match dest {
            ToDest::File(path) => {
                let spec = RecSpec {
                    sample_rate: engine_rate(),
                    channels: save.channels.max(1),
                    kind: sample_kind(save.bits),
                };
                match crate::export::start_with(&path, spec, WriterOpts::default()) {
                    Ok((ring, handle)) => {
                        let samples = frames * ch;
                        let fed = std::thread::Builder::new()
                            .name("blast-buf-save".into())
                            .spawn(move || feed(ring, data.into_vec(), samples));
                        match fed {
                            Ok(_) => self.engine_state.saves.writers.push(Writer {
                                buffer: name,
                                shown: written,
                                handle,
                            }),
                            Err(e) => {
                                handle.request_stop();
                                self.engine_state.saves.notices.push(format!(
                                    "buf {name}: couldn't write {}: {e}",
                                    path.display()
                                ));
                            }
                        }
                    }
                    Err(e) => self.engine_state.saves.notices.push(format!(
                        "buf {name}: couldn't write {}: {e}",
                        path.display()
                    )),
                }
                Vec::new()
            }
            ToDest::Track(track) => {
                let mut samples = data.into_vec();
                samples.truncate(frames * ch);
                let file = AudioFile::new(
                    &track,
                    "wav",
                    engine_rate(),
                    u32::from(save.channels.max(1)),
                    32,
                    samples,
                );
                let cmds = if self.engine_state.tracks.contains_key(&track) {
                    Ok(self.replace_track(&track, file, None))
                } else {
                    self.engine_state
                        .add_track_command(file, None)
                        .map(|cmd| vec![cmd])
                };
                match cmds {
                    Ok(cmds) => {
                        self.engine_state
                            .saves
                            .notices
                            .push(format!("buf {name}: track {track} ({})", secs_text(frames)));
                        cmds
                    }
                    Err(e) => {
                        self.engine_state
                            .saves
                            .notices
                            .push(format!("buf {name}: couldn't make track {track}: {e}"));
                        Vec::new()
                    }
                }
            }
        }
    }

    /// Lines about takes that went out since the last call: tracks made,
    /// and what the file writers report ("buf take: wrote jam.wav (0:42, 8
    /// MB)"). A writer that is done and has been heard is forgotten.
    pub fn buffer_notices(&mut self) -> Vec<String> {
        let saves = &mut self.engine_state.saves;
        let mut out = std::mem::take(&mut saves.notices);
        saves.writers.retain(|w| {
            // a writer's lines come with its end; read them after the check,
            // so none is lost between the two
            let running = w.handle.is_running();
            for line in w.handle.take_notices() {
                let rest = line.strip_prefix("rec: ").unwrap_or(&line);
                out.push(match rest.strip_prefix("failed: ") {
                    Some(why) => format!("buf {}: couldn't write {}: {why}", w.buffer, w.shown),
                    None => format!("buf {}: {rest}", w.buffer),
                });
            }
            running
        });
        out
    }

    /// At quit: stops the takes that are running with `to=`, collects their
    /// copies and writes them, waiting up to `timeout` in all. The notices
    /// are for `buffer_notices`.
    pub fn finish_buffers(&mut self, sender: &mut CmdSender, rx: &mut EngineRx, timeout: Duration) {
        let started = Instant::now();
        let mut stops: Vec<Command> = Vec::new();
        let mut expected: Vec<u32> = Vec::new();
        for b in self.engine_state.buffers.values_mut() {
            let has_pending = self
                .engine_state
                .saves
                .pending
                .values()
                .any(|p| p.slot == b.id.0);
            if b.take != Take::Idle && b.to.is_some() && has_pending {
                b.take = Take::Idle;
                expected.push(b.id.0);
                stops.push(Command::BufferEdit(BufferEditArgs::new(
                    b.id.0,
                    BufOp::Stop,
                )));
            }
        }
        let mut last_progress = Instant::now();
        loop {
            // the queue may be full; the engine makes room as it renders
            while !stops.is_empty() {
                match sender.try_push(stops.remove(0)) {
                    Ok(()) => last_progress = Instant::now(),
                    Err(cmd) => {
                        stops.insert(0, cmd);
                        break;
                    }
                }
            }
            for (slot, ticket, data, frames) in rx.drain_saves() {
                expected.retain(|&s| s != slot);
                // a replaced track has no voices to tell: the session is ending
                let _ = self.note_buffer_save(slot, ticket, data, frames);
                last_progress = Instant::now();
            }
            // a take whose start was still waiting never answers: give up
            // when nothing has come for a while
            let quiet = stops.is_empty() && last_progress.elapsed() > Duration::from_millis(500);
            if expected.is_empty() || quiet || started.elapsed() >= timeout {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        for w in self.engine_state.saves.writers.clone() {
            w.handle.wait(timeout.saturating_sub(started.elapsed()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_box_too_big_e709() {
        // more than the address space: refused without trying
        let e = alloc_save(usize::MAX / 2).err().unwrap();
        let (code, msg) = no_memory("b", e);
        assert_eq!(code.as_str(), "E709");
        assert!(msg.starts_with("not enough memory for buffer b ("), "{msg}");
        assert_eq!(alloc_save(8).unwrap().len(), 8);
    }

    #[test]
    fn to_classifies_file_track_none() {
        let home = Some(std::ffi::OsString::from("/home/x"));
        assert_eq!(BufTo::parse("", home.clone()), None);
        let file = |p: &str| ToDest::File(PathBuf::from(p));
        assert_eq!(BufTo::parse("a.wav", None).unwrap().dest, file("a.wav"));
        assert_eq!(BufTo::parse("d/a", None).unwrap().dest, file("d/a"));
        assert_eq!(
            BufTo::parse("~/a.wav", home).unwrap().dest,
            file("/home/x/a.wav")
        );
        assert_eq!(
            BufTo::parse("kick take", None).unwrap().dest,
            ToDest::Track("kick take".into())
        );
    }
}
