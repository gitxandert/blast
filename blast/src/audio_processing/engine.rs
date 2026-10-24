use std::{cell::RefCell, collections::HashMap, rc::Rc};

use alsa_sys::*;

use crate::audio_processing::{
    blast_time::blast_time::{TempoMode, TempoState, clock},
    commands::*, // too many to list
    processes::*,
};
use crate::file_parsing::decode_helpers::AudioFile;

// audio engine
//
pub struct Conductor {
    voices: Vec<Voice>,
    groups: Vec<Group>,
    tempo_cons: Vec<Rc<RefCell<TempoState>>>,
    out_channels: usize,
    tracks: Vec<AudioFile>,
}

impl Conductor {
    pub fn prepare(out_channels: usize, tracks: HashMap<String, AudioFile>) -> Self {
        Self {
            voices: Vec::<Voice>::new(),
            groups: Vec::<Group>::new(),
            tempo_cons: Vec::<Rc<RefCell<TempoState>>>::new(),
            out_channels,
            tracks: tracks.into_values().collect(),
        }
    }

    pub fn coordinate(
        &mut self,
        areas_ptr: *const snd_pcm_channel_area_t,
        offset: snd_pcm_uframes_t,
        frames: snd_pcm_uframes_t,
    ) {
        unsafe {
            let areas = std::slice::from_raw_parts(areas_ptr, self.out_channels);

            for f in 0..frames {
                // per-frame work (processes, tempos) runs once
                // per frame, not once per channel
                for voice in &mut self.voices {
                    voice.tick();
                }

                for group in &mut self.groups {
                    group.tick();
                }

                // TempoContexts belong to the Conductor; ticked here,
                // outside the Voice/Group loops, so nothing else
                // holds a borrow of them
                for tc in &self.tempo_cons {
                    tc.borrow_mut().update(1.0);
                }

                for ch in 0..self.out_channels {
                    let a = &areas[ch];
                    let base = a.addr as *mut u8;

                    // ALSA channel area addressing
                    let bit_offset = a.first as isize + (offset + f) as isize * a.step as isize;
                    let byte_offset = bit_offset / 8;

                    let sample_ptr = base.offset(byte_offset) as *mut i16;

                    // mix in f32 and clip once on write, so summing
                    // Voices can't overflow i16
                    let mut mix = 0f32;

                    for voice in &mut self.voices {
                        if voice.state.active {
                            voice.process(&mut mix, ch);
                        }
                    }

                    for group in &mut self.groups {
                        if group.state.active {
                            group.process(&mut mix, ch);
                        }
                    }

                    *sample_ptr = mix.clamp(i16::MIN as f32, i16::MAX as f32) as i16;
                }

                // advance once per frame, after every channel is mixed
                for voice in &mut self.voices {
                    voice.advance();
                }

                for group in &mut self.groups {
                    group.advance();
                }

                clock::advance(1);
            }
        }
    }

    pub fn apply(&mut self, cmd: Command) {
        match cmd {
            Command::Load(args) => self.load(args),
            Command::Start(args) => self.start(args),
            Command::Pause(args) => self.pause(args),
            Command::Resume(args) => self.resume(args),
            Command::Stop(args) => self.stop(args),
            Command::Unload(args) => self.unload(args),
            Command::Velocity(args) => self.velocity(args),
            Command::Group(args) => self.group(args),
            Command::Tc(args) => self.tempo_context(args),
            Command::Seq(args) => self.seq(args),
            Command::Quit(_) => unsafe {
                libc::raise(libc::SIGTERM);
            },
        }
    }

    fn load(&mut self, args: LoadArgs) {
        let track = self.tracks.get(args.track_idx).unwrap();
        let tempo_state = self.tempo_from_repr(args.tempo_repr);
        self.voices.push(Voice::new(track, tempo_state));
    }

    fn start(&mut self, args: StartArgs) {
        match args.idx {
            Idx::Voice(idx) => {
                let voice: &mut Voice = self.voices.get_mut(idx).unwrap();
                voice.start();
            }
            Idx::GroupVoice(g, v) => {
                let voice: &mut Voice = self.groups[g].voices.get_mut(v).unwrap();
                voice.start();
            }
            Idx::Group(idx) => {
                let group: &mut Group = self.groups.get_mut(idx).unwrap();
                group.start();
            }
            Idx::Tempo(idx) => {
                let mut tc = self.tempo_cons.get(idx).unwrap().borrow_mut();
                tc.start();
            }
            _ => (),
        }
    }

    fn pause(&mut self, args: PauseArgs) {
        match args.idx {
            Idx::Voice(idx) => {
                let voice: &mut Voice = self.voices.get_mut(idx).unwrap();
                voice.pause();
            }
            Idx::GroupVoice(g, v) => {
                let voice: &mut Voice = self.groups[g].voices.get_mut(v).unwrap();
                voice.pause();
            }
            Idx::Group(idx) => {
                let group: &mut Group = self.groups.get_mut(idx).unwrap();
                group.pause();
            }
            Idx::Tempo(idx) => {
                let mut tc = self.tempo_cons.get(idx).unwrap().borrow_mut();
                tc.pause();
            }
            _ => (),
        }
    }

    fn resume(&mut self, args: ResumeArgs) {
        match args.idx {
            Idx::Voice(idx) => {
                let voice: &mut Voice = self.voices.get_mut(idx).unwrap();
                voice.resume();
            }
            Idx::GroupVoice(g, v) => {
                let voice: &mut Voice = self.groups[g].voices.get_mut(v).unwrap();
                voice.resume();
            }
            Idx::Group(idx) => {
                let group: &mut Group = self.groups.get_mut(idx).unwrap();
                group.resume();
            }
            Idx::Tempo(idx) => {
                let mut tc = self.tempo_cons.get(idx).unwrap().borrow_mut();
                tc.resume();
            }
            _ => (),
        }
    }

    fn stop(&mut self, args: StopArgs) {
        match args.idx {
            Idx::Voice(idx) => {
                let voice: &mut Voice = self.voices.get_mut(idx).unwrap();
                voice.stop();
            }
            Idx::GroupVoice(g, v) => {
                let voice: &mut Voice = self.groups[g].voices.get_mut(v).unwrap();
                voice.stop();
            }
            Idx::Group(idx) => {
                let group: &mut Group = self.groups.get_mut(idx).unwrap();
                group.stop();
            }
            Idx::Tempo(idx) => {
                let mut tc = self.tempo_cons.get(idx).unwrap().borrow_mut();
                tc.stop();
            }
            _ => (),
        }
    }

    fn unload(&mut self, args: UnloadArgs) {
        match args.idx {
            Idx::Voice(v) => {
                self.voices.remove(v);
            }
            Idx::GroupVoice(g, v) => {
                self.groups[g].voices.remove(v);
            }
            _ => (), // will only be Voice or GroupVoice
        }
    }

    fn velocity(&mut self, args: VelocityArgs) {
        let voice: &mut Voice = match args.idx {
            Idx::Voice(v) => self.voices.get_mut(v).unwrap(),
            Idx::GroupVoice(g, v) => self.groups[g].voices.get_mut(v).unwrap(),
            _ => return, // will only be Voice or GroupVoice
        };
        voice.state.velocity = args.val;
    }

    fn group(&mut self, args: GroupArgs) {
        let tempo = self.tempo_from_repr(args.tempo);
        let vs_fs_ps = args.vs_fs_ps;

        // vs_fs_ps is in Group order; remove Voices in reverse idx
        // order so each removal doesn't shift the ones still to come,
        // then put each Voice in its slot in the Group
        let mut order: Vec<usize> = (0..vs_fs_ps.len()).collect();
        order.sort_by(|a, b| vs_fs_ps[*b].0.cmp(&vs_fs_ps[*a].0));

        let mut slots: Vec<Option<Voice>> = vs_fs_ps.iter().map(|_| None).collect();
        for pos in order {
            let (idx, update_tempo, p_ids) = &vs_fs_ps[pos];
            // move Voices out of conductor.voices into group.voices
            let mut voice = self.voices.remove(*idx);
            if *update_tempo {
                // refer to Group TempoState
                voice.state.tempo = Rc::clone(&tempo);
                for p in p_ids {
                    // these Processes also refer to the
                    // Group TempoState
                    let process = &mut voice.processes[*p];
                    process.update_tempo(Rc::clone(&tempo));
                }
            }
            slots[pos] = Some(voice);
        }
        let voices: Vec<Voice> = slots.into_iter().map(|v| v.unwrap()).collect();

        let group = Group::new(voices, tempo);
        self.groups.push(group);
    }

    fn tempo_context(&mut self, args: TcArgs) {
        let tempo_state = self.tempo_from_repr(args.tempo);
        self.tempo_cons.push(tempo_state);
    }

    // Processes
    //
    fn seq(&mut self, args: SeqArgs) {
        let tempo = match args.idx {
            // a Voice/TBD reference is to the Seq's own Voice (-t v);
            // tempo_from_repr only knows Conductor.voices, so take
            // a grouped Voice's TempoState directly
            Idx::GroupVoice(g, v)
                if !args.tempo.owned
                    && (args.tempo.mode == TempoMode::Voice
                        || args.tempo.mode == TempoMode::TBD) =>
            {
                Rc::clone(&self.groups[g].voices[v].state.tempo)
            }
            _ => self.tempo_from_repr(TempoRepr::clone(&args.tempo)),
        };
        let state = SeqState {
            active: true,
            tempo: Rc::clone(&tempo),
            period: args.period,
            steps: args.steps,
            chance: args.chance,
            jit: args.jit,
            rng: args.rng,
            idx: 0,
            last: None,
            epoch: tempo.borrow().epoch,
        };

        match args.idx {
            Idx::Voice(v) => {
                let voice: &mut Voice = self.voices.get_mut(v).unwrap();
                voice.processes.push(Process::Seq(Seq { state }));
                if args.tempo.mode == TempoMode::Process {
                    // Voice::start only starts proc_tempi that exist
                    // at the time, so start this one if the Voice is
                    // already playing
                    if voice.state.active {
                        tempo.borrow_mut().start();
                    }
                    voice.proc_tempi.push(tempo);
                }
            }
            Idx::GroupVoice(g, v) => {
                let voice: &mut Voice = self.groups[g].voices.get_mut(v).unwrap();
                voice.processes.push(Process::Seq(Seq { state }));
                if args.tempo.mode == TempoMode::Process {
                    if voice.state.active {
                        tempo.borrow_mut().start();
                    }
                    voice.proc_tempi.push(tempo);
                }
            }
            Idx::Group(g) => {
                let group: &mut Group = self.groups.get_mut(g).unwrap();
                group.processes.push(Process::Seq(Seq { state }));
                if args.tempo.mode == TempoMode::Process {
                    group.proc_tempi.push(tempo);
                }
            }
            _ => (), // will only be Voice or Group
        }
    }

    // helpers
    //
    fn tempo_from_repr(&self, tr: TempoRepr) -> Rc<RefCell<TempoState>> {
        // either create (and init) a new TempoState,
        // or find the referenced one within Groups or Contexts
        let mut tempo = Rc::new(RefCell::new(TempoState::new(None)));
        if tr.owned {
            tempo.borrow_mut().init(tr.mode, tr.unit, tr.interval);
        } else {
            match tr.mode {
                // TBD here means the Voice was loaded without a tempo;
                // still share its (future) TempoState
                TempoMode::Voice | TempoMode::TBD => {
                    tempo = Rc::clone(&self.voices[tr.idx].state.tempo);
                }
                TempoMode::Group => {
                    tempo = Rc::clone(&self.groups[tr.idx].state.tempo);
                }
                TempoMode::Context => {
                    tempo = Rc::clone(&self.tempo_cons[tr.idx]);
                }
                // Process will never borrow from another Process
                TempoMode::Process => (),
            }
        }

        tempo
    }
}

pub struct VoiceState {
    pub active: bool,
    pub position: f32,
    pub end: usize,
    pub velocity: f32,
    pub gain: f32,
    pub tempo: Rc<RefCell<TempoState>>,
}

impl VoiceState {
    // whether position is within the playable frames 0..=end
    fn in_bounds(&self) -> bool {
        self.position >= 0.0 && self.position <= self.end as f32
    }
}

// instead of samples, Voices have a Function,
// which are either some type of Wave or a collection of samples
// (not used yet; see the rewrite note on Voice)
#[allow(dead_code)]
enum Function {
    Track { samples: Vec<i16> },
    Wave { form: WaveForm },
}

#[allow(dead_code)]
enum WaveForm {
    Custom,
    Sine,
    Triangle,
    Square,
}

// rewrite later to:
// pub struct Voice {
//      function: Function,
//      channels: usize,
//      processes: Vec<Process>,
//      proc_tempi: Vec<Rc<RefCell<TempoState>>>,
// }
// the current implemntation of VoiceState will become TrackState;
// Waves get their own WaveState
pub struct Voice {
    samples: Vec<i16>,
    channels: usize,
    pub state: VoiceState,
    processes: Vec<Process>,
    proc_tempi: Vec<Rc<RefCell<TempoState>>>, // TempoMode::Process
}

impl Voice {
    fn new(af: &AudioFile, tempo_state: Rc<RefCell<TempoState>>) -> Self {
        let voice_state = VoiceState {
            active: false,
            position: 0.0,
            // an empty track has no frames; load_assets skips those,
            // but don't underflow if one gets through
            end: (af.samples.len() / (af.num_channels as usize).max(1)).saturating_sub(1),
            velocity: 1.0,
            gain: 1.0,
            tempo: tempo_state,
        };

        Self {
            // tracks are f32 now (scaffold step 0); voices stay i16 until E1
            samples: af
                .samples
                .iter()
                .map(|&s| (s * 32768.0).clamp(-32768.0, 32767.0) as i16)
                .collect(),
            channels: af.num_channels as usize,
            state: voice_state,
            processes: Vec::<Process>::new(),
            proc_tempi: Vec::<Rc<RefCell<TempoState>>>::new(),
        }
    }

    fn start(&mut self) {
        let state = &mut self.state;
        state.active = true;

        for p in &mut self.processes {
            p.reset();
        }

        let mut ts = state.tempo.borrow_mut();
        if ts.mode == TempoMode::Voice || ts.mode == TempoMode::TBD {
            ts.start();
        } else {
            if ts.active == false {
                println!("\nWarn: Tempo not active for Voice");
            }
        }

        for tempo_state in &mut self.proc_tempi {
            let mut ts = tempo_state.borrow_mut();
            ts.start();
        }

        state.position = match state.velocity >= 0.0 {
            true => 0.0,
            false => state.end as f32,
        };
    }

    fn pause(&mut self) {
        self.state.active = false;
    }

    fn resume(&mut self) {
        self.state.active = true;

        let ts = self.state.tempo.borrow();
        if ts.mode != TempoMode::Voice {
            if ts.active == false {
                println!("\nWarn: Tempo not active for Voice");
            }
        }
    }

    fn stop(&mut self) {
        let state = &mut self.state;
        state.active = false;

        for p in &mut self.processes {
            p.reset();
        }

        // same ownership rule as start()
        let mut ts = state.tempo.borrow_mut();
        if ts.mode == TempoMode::Voice || ts.mode == TempoMode::TBD {
            ts.stop();
        }

        for tempo_state in &self.proc_tempi {
            let mut ts = tempo_state.borrow_mut();
            ts.active = false;
            ts.reset();
        }

        state.position = match state.velocity >= 0.0 {
            true => 0.0,
            false => state.end as f32,
        };
    }

    // run Processes and update TempoStates;
    // called once per frame, before process()
    fn tick(&mut self) {
        if !self.state.active {
            return;
        }

        let state = &mut self.state;

        // processing
        for p in &mut self.processes {
            p.process(state);
        }

        let mut own_tempo = state.tempo.borrow_mut();
        if own_tempo.mode == TempoMode::Voice || own_tempo.mode == TempoMode::TBD {
            // only update own TempoState if it belongs to this Voice
            own_tempo.update(1.0);
        }

        for tempo_state in &mut self.proc_tempi {
            let mut ts = tempo_state.borrow_mut();
            ts.update(1.0);
        }
    }

    // mix one channel of the current frame into acc;
    // called once per channel
    fn process(&mut self, acc: &mut f32, mut ch: usize) {
        if !self.state.active {
            return;
        }

        let state = &mut self.state;

        // plays frames 0..=end in either direction; check the f32
        // position before casting, since a negative position
        // would saturate to 0 as usize
        if !state.in_bounds() || self.samples.len() < self.channels {
            return;
        }
        let idx = state.position as usize;

        // if there are more output channels than the track has
        // recorded into, then skip putting info into the extra
        // channels, unless the track is mono and there are two
        // output channels, in which case, output the same samples
        // through both channels
        //
        // this is a hack; def need a better routing system later
        if self.channels == 1 {
            if ch < 2 {
                ch = 0;
            } else {
                return;
            }
        } else if ch >= self.channels {
            return;
        }

        // linear interpolation
        let s0 = self.samples[(idx * self.channels) + (ch % self.channels)] as f32;
        let sample = if state.velocity != 1.0 {
            let frac = state.position.fract();
            // the last frame has no next frame to interpolate toward
            let next = if idx < state.end { idx + 1 } else { idx };
            let s1 = self.samples[(next * self.channels) + (ch % self.channels)] as f32;
            s0 * (1.0 - frac) + s1 * frac
        } else {
            s0
        };

        *acc += sample * state.gain;
    }

    // move position; called once per frame, after process()
    fn advance(&mut self) {
        if !self.state.active {
            return;
        }

        let state = &mut self.state;

        // stop moving once playback has run off either end
        if !state.in_bounds() {
            return;
        }

        state.position += state.velocity;
    }
}

pub struct GroupState {
    pub active: bool,
    pub gain: f32,
    pub tempo: Rc<RefCell<TempoState>>,
}

pub struct Group {
    pub state: GroupState,
    pub voices: Vec<Voice>,
    pub processes: Vec<Process>,
    proc_tempi: Vec<Rc<RefCell<TempoState>>>, // TempoMode::Process
}

impl Group {
    fn new(voices: Vec<Voice>, tempo: Rc<RefCell<TempoState>>) -> Self {
        let state = GroupState {
            active: false,
            gain: 1.0,
            tempo,
        };

        Self {
            state,
            voices,
            processes: Vec::<Process>::new(),
            proc_tempi: Vec::<Rc<RefCell<TempoState>>>::new(),
        }
    }

    fn start(&mut self) {
        let state = &mut self.state;
        state.active = true;

        {
            let mut ts = state.tempo.borrow_mut();
            if ts.mode == TempoMode::Group {
                ts.active = true;
                ts.reset();
            } else {
                if ts.active == false {
                    println!("\nWarn: Tempo not active for Group");
                }
            }
        }

        for voice in &mut self.voices {
            voice.start();
        }

        for p in &mut self.processes {
            p.reset();
        }

        for tempo_state in &mut self.proc_tempi {
            tempo_state.borrow_mut().start();
        }
    }

    fn pause(&mut self) {
        self.state.active = false; // Voices still active, but won't
        // be doing anything since their
        // process() isn't being called
    }

    fn resume(&mut self) {
        self.state.active = true;

        let ts = self.state.tempo.borrow();
        if ts.mode == TempoMode::Context {
            if ts.active == false {
                println!("\nWarn: Tempo not active for Group");
            }
        }
    }

    fn stop(&mut self) {
        self.state.active = false;

        // reset positions, Processes and own TempoStates too,
        // so the next start begins from the top
        for voice in &mut self.voices {
            voice.stop();
        }

        for p in &mut self.processes {
            p.reset();
        }

        for tempo_state in &self.proc_tempi {
            tempo_state.borrow_mut().stop();
        }

        let mut ts = self.state.tempo.borrow_mut();
        if ts.mode == TempoMode::Group {
            ts.active = false;
            ts.reset();
        }
    }

    // run Voices, own Processes and TempoStates;
    // called once per frame
    fn tick(&mut self) {
        if !self.state.active {
            return;
        }

        for v in &mut self.voices {
            v.tick();
        }

        for p in &mut self.processes {
            p.process_group(&mut self.voices);
        }

        let mut ts = self.state.tempo.borrow_mut();
        if ts.mode == TempoMode::Group {
            ts.update(1.0);
        }

        for tempo_state in &self.proc_tempi {
            tempo_state.borrow_mut().update(1.0);
        }
    }

    // mix one channel of the current frame into acc
    fn process(&mut self, acc: &mut f32, ch: usize) {
        if !self.state.active {
            return;
        }

        for v in &mut self.voices {
            v.process(acc, ch);
        }
    }

    // advance Voices; called once per frame
    fn advance(&mut self) {
        if !self.state.active {
            return;
        }

        for v in &mut self.voices {
            v.advance();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_processing::test_util::{conductor, pcm, render};
    use crate::audio_processing::{
        blast_rand::X128P,
        blast_time::{blast_time::TempoUnit, sample_rate},
    };

    // interleaved track of `frames` frames where every sample is `val`
    fn track(name: &str, channels: u32, frames: usize, val: i16) -> AudioFile {
        AudioFile::new(
            name,
            "wav",
            48000,
            channels,
            16,
            pcm(&vec![val; frames * channels as usize]),
        )
    }

    fn tempo(mode: TempoMode, interval: f32) -> TempoRepr {
        TempoRepr {
            idx: 0,
            owned: true,
            mode,
            unit: TempoUnit::Samples,
            interval,
        }
    }

    // Conductor with one loaded (not started) Voice
    fn one_voice(out_channels: usize, af: AudioFile, tr: TempoRepr) -> Conductor {
        sample_rate::set(48000);
        let mut c = conductor(out_channels, vec![af]);
        c.apply(Command::Load(LoadArgs {
            track_idx: 0,
            tempo_repr: tr,
        }));
        c
    }

    fn start_voice(c: &mut Conductor, idx: usize) {
        c.apply(Command::Start(StartArgs {
            idx: Idx::Voice(idx),
        }));
    }

    fn voice_tempo_ticks_once_per_frame(channels: usize) {
        let mut c = one_voice(
            channels,
            track("t", channels as u32, 2000, 100),
            tempo(TempoMode::Voice, 100.0),
        );
        start_voice(&mut c, 0);

        render(&mut c, channels, 1000);

        assert_eq!(c.voices[0].state.tempo.borrow().current, 1000);
        assert_eq!(c.voices[0].state.position, 1000.0);
    }

    #[test]
    fn voice_tempo_ticks_once_per_frame_stereo() {
        voice_tempo_ticks_once_per_frame(2);
    }

    #[test]
    fn voice_tempo_ticks_once_per_frame_mono() {
        voice_tempo_ticks_once_per_frame(1);
    }

    #[test]
    fn group_tempo_ticks_once_per_frame() {
        let mut c = one_voice(2, track("t", 2, 2000, 100), tempo(TempoMode::Voice, 100.0));
        c.apply(Command::Group(GroupArgs {
            tempo: tempo(TempoMode::Group, 100.0),
            vs_fs_ps: vec![(0, true, Vec::new())],
        }));
        c.apply(Command::Start(StartArgs { idx: Idx::Group(0) }));

        render(&mut c, 2, 1000);

        let g = &c.groups[0];
        assert_eq!(g.state.tempo.borrow().current, 1000);
        assert_eq!(g.voices[0].state.position, 1000.0);
    }

    #[test]
    fn voice_start_pause_resume_stop() {
        let mut c = one_voice(2, track("t", 2, 2000, 100), tempo(TempoMode::Voice, 100.0));
        assert!(!c.voices[0].state.active);

        start_voice(&mut c, 0);
        assert!(c.voices[0].state.active);
        assert!(c.voices[0].state.tempo.borrow().active);
        render(&mut c, 2, 10);
        assert_eq!(c.voices[0].state.position, 10.0);

        c.apply(Command::Pause(PauseArgs { idx: Idx::Voice(0) }));
        assert!(!c.voices[0].state.active);
        // paused Voices don't advance or output
        let out = render(&mut c, 2, 10);
        assert_eq!(c.voices[0].state.position, 10.0);
        assert!(out.iter().all(|&s| s == 0));

        c.apply(Command::Resume(ResumeArgs { idx: Idx::Voice(0) }));
        assert!(c.voices[0].state.active);
        render(&mut c, 2, 10);
        assert_eq!(c.voices[0].state.position, 20.0);

        c.apply(Command::Stop(StopArgs { idx: Idx::Voice(0) }));
        assert!(!c.voices[0].state.active);
        assert_eq!(c.voices[0].state.position, 0.0);
        assert_eq!(c.voices[0].state.tempo.borrow().current, 0);
        assert!(!c.voices[0].state.tempo.borrow().active);

        // start resets position and tempo
        start_voice(&mut c, 0);
        render(&mut c, 2, 10);
        start_voice(&mut c, 0);
        assert_eq!(c.voices[0].state.position, 0.0);
        assert_eq!(c.voices[0].state.tempo.borrow().current, 0);
    }

    #[test]
    fn mono_voice_on_stereo_advances_once_per_frame() {
        let af = AudioFile::new("m", "wav", 48000, 1, 16, pcm(&(0..100).collect::<Vec<_>>()));
        let mut c = one_voice(2, af, tempo(TempoMode::Voice, 100.0));
        start_voice(&mut c, 0);

        let out = render(&mut c, 2, 10);

        // each mono sample is duplicated across ch0/ch1
        let expected: Vec<i16> = (0..10).flat_map(|s| [s, s]).collect();
        assert_eq!(out, expected);
        assert_eq!(c.voices[0].state.position, 10.0);
    }

    #[test]
    fn stereo_voice_on_stereo_keeps_channels() {
        // L = frame index, R = -frame index
        let samples: Vec<i16> = (0..100).flat_map(|s| [s, -s]).collect();
        let af = AudioFile::new("s", "wav", 48000, 2, 16, pcm(&samples));
        let mut c = one_voice(2, af, tempo(TempoMode::Voice, 100.0));
        start_voice(&mut c, 0);

        let out = render(&mut c, 2, 10);

        let expected: Vec<i16> = (0..10).flat_map(|s| [s, -s]).collect();
        assert_eq!(out, expected);
        assert_eq!(c.voices[0].state.position, 10.0);
    }

    // mono [10,20,30,40,50] at 1 output channel with velocity `vel`
    fn render_at_velocity(vel: f32, frames: usize) -> Vec<i16> {
        let af = AudioFile::new("v", "wav", 48000, 1, 16, pcm(&[10, 20, 30, 40, 50]));
        let mut c = one_voice(1, af, tempo(TempoMode::Voice, 100.0));
        c.apply(Command::Velocity(VelocityArgs {
            idx: Idx::Voice(0),
            val: vel,
        }));
        start_voice(&mut c, 0);
        render(&mut c, 1, frames)
    }

    #[test]
    fn forward_playback_plays_every_frame_then_stops() {
        assert_eq!(
            render_at_velocity(1.0, 8),
            vec![10, 20, 30, 40, 50, 0, 0, 0]
        );
    }

    #[test]
    fn reverse_playback_plays_every_frame_then_stops() {
        assert_eq!(
            render_at_velocity(-1.0, 8),
            vec![50, 40, 30, 20, 10, 0, 0, 0]
        );
    }

    #[test]
    fn half_speed_interpolates() {
        assert_eq!(
            render_at_velocity(0.5, 11),
            vec![10, 15, 20, 25, 30, 35, 40, 45, 50, 0, 0]
        );
    }

    #[test]
    fn reverse_half_speed_interpolates() {
        assert_eq!(
            render_at_velocity(-0.5, 11),
            vec![50, 45, 40, 35, 30, 25, 20, 15, 10, 0, 0]
        );
    }

    // two started mono Voices of constant `a` and `b` on 1 output channel
    fn render_two_voices(a: i16, b: i16) -> Vec<i16> {
        sample_rate::set(48000);
        let mut c = conductor(1, vec![track("a", 1, 100, a), track("b", 1, 100, b)]);
        // track order comes from a HashMap, so look the indices up
        let ia = c.tracks.iter().position(|t| t.file_name == "a").unwrap();
        let ib = c.tracks.iter().position(|t| t.file_name == "b").unwrap();
        c.apply(Command::Load(LoadArgs {
            track_idx: ia,
            tempo_repr: tempo(TempoMode::Voice, 100.0),
        }));
        c.apply(Command::Load(LoadArgs {
            track_idx: ib,
            tempo_repr: tempo(TempoMode::Voice, 100.0),
        }));
        start_voice(&mut c, 0);
        start_voice(&mut c, 1);
        render(&mut c, 1, 10)
    }

    #[test]
    fn mix_clips_at_i16_max() {
        assert!(
            render_two_voices(30000, 30000)
                .iter()
                .all(|&s| s == i16::MAX)
        );
    }

    #[test]
    fn mix_clips_at_i16_min() {
        assert!(
            render_two_voices(-30000, -30000)
                .iter()
                .all(|&s| s == i16::MIN)
        );
    }

    #[test]
    fn mix_sums_small_values_exactly() {
        assert!(render_two_voices(1000, 2000).iter().all(|&s| s == 3000));
        assert!(render_two_voices(1000, -3000).iter().all(|&s| s == -2000));
    }

    #[test]
    fn tempo_context_ticks_once_per_frame_while_active() {
        sample_rate::set(48000);
        let mut c = conductor(2, vec![track("t", 2, 2000, 100)]);
        c.apply(Command::Tc(TcArgs {
            tempo: tempo(TempoMode::Context, 100.0),
        }));
        // a Voice synced to the context shares its TempoState
        let mut synced = tempo(TempoMode::Context, 100.0);
        synced.owned = false;
        c.apply(Command::Load(LoadArgs {
            track_idx: 0,
            tempo_repr: synced,
        }));

        // not started yet
        render(&mut c, 2, 10);
        assert_eq!(c.tempo_cons[0].borrow().current, 0);

        c.apply(Command::Start(StartArgs { idx: Idx::Tempo(0) }));
        start_voice(&mut c, 0);
        render(&mut c, 2, 100);
        assert_eq!(c.tempo_cons[0].borrow().current, 100);
        assert_eq!(c.voices[0].state.tempo.borrow().current, 100);

        c.apply(Command::Pause(PauseArgs { idx: Idx::Tempo(0) }));
        render(&mut c, 2, 50);
        assert_eq!(c.tempo_cons[0].borrow().current, 100);

        c.apply(Command::Resume(ResumeArgs { idx: Idx::Tempo(0) }));
        render(&mut c, 2, 50);
        assert_eq!(c.tempo_cons[0].borrow().current, 150);

        c.apply(Command::Stop(StopArgs { idx: Idx::Tempo(0) }));
        render(&mut c, 2, 50);
        assert_eq!(c.tempo_cons[0].borrow().current, 0);
    }

    // Group of two mono Voices (samples = frame index) with a Seq
    // that retriggers on steps 0 and 2 of a 4-step period at s:100
    fn group_with_seq(seq_tempo: TempoRepr) -> Conductor {
        sample_rate::set(48000);
        let mut c = conductor(
            1,
            vec![AudioFile::new(
                "t",
                "wav",
                48000,
                1,
                16,
                pcm(&(0..1000).collect::<Vec<_>>()),
            )],
        );
        c.apply(Command::Load(LoadArgs {
            track_idx: 0,
            tempo_repr: tempo(TempoMode::Voice, 100.0),
        }));
        c.apply(Command::Load(LoadArgs {
            track_idx: 0,
            tempo_repr: tempo(TempoMode::Voice, 100.0),
        }));
        c.apply(Command::Group(GroupArgs {
            tempo: tempo(TempoMode::Group, 100.0),
            vs_fs_ps: vec![(1, true, Vec::new()), (0, true, Vec::new())],
        }));
        c.apply(Command::Seq(SeqArgs {
            idx: Idx::Group(0),
            tempo: seq_tempo,
            period: 4,
            steps: vec![0.0, 2.0],
            chance: vec![100.0, 100.0],
            jit: vec![0.0, 0.0],
            rng: X128P::new(1),
        }));
        c.apply(Command::Start(StartArgs { idx: Idx::Group(0) }));
        c
    }

    fn assert_group_seq_retriggers(mut c: Conductor) {
        let out = render(&mut c, 1, 450);

        // both Voices restart on frames 200 and 400
        assert_eq!(out[199], 2 * 199);
        assert_eq!(out[200], 0);
        assert_eq!(out[201], 2 * 1);
        assert_eq!(out[399], 2 * 199);
        assert_eq!(out[400], 0);
        for v in &c.groups[0].voices {
            assert_eq!(v.state.position, 50.0);
        }
    }

    #[test]
    fn group_seq_with_own_tempo_retriggers_all_voices() {
        assert_group_seq_retriggers(group_with_seq(tempo(TempoMode::Process, 100.0)));
    }

    #[test]
    fn group_seq_on_group_tempo_retriggers_all_voices() {
        let mut tr = tempo(TempoMode::Group, 100.0);
        tr.owned = false;
        assert_group_seq_retriggers(group_with_seq(tr));
    }

    #[test]
    fn voice_seq_retriggers_voice() {
        let af = AudioFile::new(
            "t",
            "wav",
            48000,
            1,
            16,
            pcm(&(0..1000).collect::<Vec<_>>()),
        );
        let mut c = one_voice(1, af, tempo(TempoMode::Voice, 100.0));
        c.apply(Command::Seq(SeqArgs {
            idx: Idx::Voice(0),
            tempo: tempo(TempoMode::Process, 100.0),
            period: 4,
            steps: vec![0.0, 2.0],
            chance: vec![100.0, 100.0],
            jit: vec![0.0, 0.0],
            rng: X128P::new(1),
        }));
        start_voice(&mut c, 0);

        let out = render(&mut c, 1, 450);
        assert_eq!(out[199], 199);
        assert_eq!(out[200], 0);
        assert_eq!(out[400], 0);
        assert_eq!(c.voices[0].state.position, 50.0);
    }

    #[test]
    fn group_stop_stops_seq_tempo() {
        let mut c = group_with_seq(tempo(TempoMode::Process, 100.0));
        render(&mut c, 1, 10);
        assert_eq!(c.groups[0].proc_tempi[0].borrow().current, 10);

        c.apply(Command::Stop(StopArgs { idx: Idx::Group(0) }));
        assert!(!c.groups[0].proc_tempi[0].borrow().active);
        assert_eq!(c.groups[0].proc_tempi[0].borrow().current, 0);
    }

    #[test]
    fn stereo_voice_mixes_its_samples() {
        let mut c = one_voice(2, track("t", 2, 100, 1234), tempo(TempoMode::Voice, 100.0));
        start_voice(&mut c, 0);

        let out = render(&mut c, 2, 50);
        assert!(out.iter().all(|&s| s == 1234));
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;
    use crate::audio_processing::blast_time::sample_rate;
    use crate::audio_processing::test_util::{render, run, setup, track};

    fn seq_tempo(p: &Process) -> Rc<RefCell<TempoState>> {
        match p {
            Process::Seq(s) => Rc::clone(&s.state.tempo),
        }
    }

    // tempo references (B15)

    #[test]
    fn seq_voice_tempo_shares_its_voice_rc() {
        sample_rate::set(48000);
        let (mut cp, mut c) = setup(1, vec![track("kick", 1, 16), track("snare", 2, 16)]);
        run(&mut cp, &mut c, "load kick");
        run(&mut cp, &mut c, "load snare -t b:120");
        run(&mut cp, &mut c, "seq snare -t v -s 0");

        let seq = seq_tempo(&c.voices[1].processes[0]);
        assert!(Rc::ptr_eq(&seq, &c.voices[1].state.tempo));
        assert!(!Rc::ptr_eq(&seq, &c.voices[0].state.tempo));
    }

    #[test]
    fn seq_tbd_voice_tempo_shares_its_voice_rc() {
        sample_rate::set(48000);
        let (mut cp, mut c) = setup(1, vec![track("kick", 1, 16), track("snare", 2, 16)]);
        run(&mut cp, &mut c, "load kick");
        run(&mut cp, &mut c, "load snare");
        run(&mut cp, &mut c, "seq snare -t v -s 0");

        let seq = seq_tempo(&c.voices[1].processes[0]);
        assert!(Rc::ptr_eq(&seq, &c.voices[1].state.tempo));
    }

    #[test]
    fn group_tempo_refs_share_the_right_group_rc() {
        sample_rate::set(48000);
        let (mut cp, mut c) = setup(
            1,
            vec![
                track("a", 1, 16),
                track("b", 2, 16),
                track("c", 3, 16),
                track("d", 4, 16),
            ],
        );
        run(&mut cp, &mut c, "load a");
        run(&mut cp, &mut c, "load b");
        run(&mut cp, &mut c, "load c");
        run(&mut cp, &mut c, "group g1 -v a");
        run(&mut cp, &mut c, "group g2 -v b");
        run(&mut cp, &mut c, "seq c -t g:g2 -s 0");
        run(&mut cp, &mut c, "load d -t g:g1");

        // c is voices[0], d is voices[1]
        let seq = seq_tempo(&c.voices[0].processes[0]);
        assert!(Rc::ptr_eq(&seq, &c.groups[1].state.tempo));
        assert!(Rc::ptr_eq(
            &c.voices[1].state.tempo,
            &c.groups[0].state.tempo
        ));
    }

    #[test]
    fn context_tempo_refs_share_the_context_rc() {
        sample_rate::set(48000);
        let (mut cp, mut c) = setup(1, vec![track("a", 1, 16)]);
        run(&mut cp, &mut c, "tc c1 b:120");
        run(&mut cp, &mut c, "tc c2 b:90");
        run(&mut cp, &mut c, "load a -t c:c2");
        run(&mut cp, &mut c, "seq a -t c:c1 -s 0");

        assert!(Rc::ptr_eq(&c.voices[0].state.tempo, &c.tempo_cons[1]));
        assert!(Rc::ptr_eq(
            &seq_tempo(&c.voices[0].processes[0]),
            &c.tempo_cons[0]
        ));
    }

    // grouping

    // tracks are constant-valued, so samples[0] identifies a Voice
    fn id(v: &Voice) -> i16 {
        v.samples[0]
    }

    #[test]
    fn group_voices_follow_mirror_order() {
        sample_rate::set(48000);
        for (cmd, want) in [("group g -v c,a", [3, 1]), ("group g -v a,c", [1, 3])] {
            let (mut cp, mut c) = setup(
                1,
                vec![track("a", 1, 16), track("b", 2, 16), track("c", 3, 16)],
            );
            run(&mut cp, &mut c, "load a");
            run(&mut cp, &mut c, "load b");
            run(&mut cp, &mut c, "load c");
            run(&mut cp, &mut c, cmd);

            let g = &c.groups[0];
            assert_eq!(g.voices.len(), 2);
            assert_eq!(id(&g.voices[0]), want[0]);
            assert_eq!(id(&g.voices[1]), want[1]);
            assert_eq!(c.voices.len(), 1);
            assert_eq!(id(&c.voices[0]), 2);
        }
    }

    #[test]
    fn grouped_voices_play_through_the_group() {
        sample_rate::set(48000);
        let (mut cp, mut c) = setup(1, vec![track("a", 100, 16), track("b", 20, 16)]);
        run(&mut cp, &mut c, "load a");
        run(&mut cp, &mut c, "load b");
        run(&mut cp, &mut c, "group g -v b,a");
        run(&mut cp, &mut c, "start -g g");

        let out = render(&mut c, 1, 4);
        assert_eq!(out[0], 120);
    }

    // addressing grouped voices (B04)

    fn grouped() -> (CmdProcessor, Conductor) {
        sample_rate::set(48000);
        let (mut cp, mut c) = setup(
            1,
            vec![track("a", 1, 16), track("b", 2, 16), track("c", 3, 16)],
        );
        run(&mut cp, &mut c, "load a");
        run(&mut cp, &mut c, "load b -t b:120");
        run(&mut cp, &mut c, "load c");
        run(&mut cp, &mut c, "group g -v a,b");
        (cp, c)
    }

    #[test]
    fn velocity_reaches_group_voice() {
        let (mut cp, mut c) = grouped();
        run(&mut cp, &mut c, "velocity g.b 2");

        assert_eq!(id(&c.groups[0].voices[1]), 2);
        assert_eq!(c.groups[0].voices[1].state.velocity, 2.0);
        assert_eq!(c.groups[0].voices[0].state.velocity, 1.0);
        assert_eq!(c.voices[0].state.velocity, 1.0);

        run(&mut cp, &mut c, "velocity c 0.5");
        assert_eq!(c.voices[0].state.velocity, 0.5);
    }

    #[test]
    fn start_stop_reach_group_voice() {
        let (mut cp, mut c) = grouped();
        run(&mut cp, &mut c, "start -v g.a");
        assert!(c.groups[0].voices[0].state.active);
        assert!(!c.groups[0].voices[1].state.active);
        assert!(!c.voices[0].state.active);

        run(&mut cp, &mut c, "pause -v g.a");
        assert!(!c.groups[0].voices[0].state.active);
        run(&mut cp, &mut c, "resume -v g.a");
        assert!(c.groups[0].voices[0].state.active);
        run(&mut cp, &mut c, "stop -v g.a");
        assert!(!c.groups[0].voices[0].state.active);
    }

    #[test]
    fn seq_reaches_group_voice() {
        let (mut cp, mut c) = grouped();
        run(&mut cp, &mut c, "seq g.b -t v -s 0");

        let b = &c.groups[0].voices[1];
        assert_eq!(b.processes.len(), 1);
        assert!(Rc::ptr_eq(&seq_tempo(&b.processes[0]), &b.state.tempo));
        assert!(c.groups[0].voices[0].processes.is_empty());
        assert!(c.voices[0].processes.is_empty());
    }

    // group tempo (B12)

    #[test]
    fn group_shares_context_rc() {
        sample_rate::set(48000);
        let (mut cp, mut c) = setup(1, vec![track("a", 1, 16)]);
        run(&mut cp, &mut c, "tc other b:60");
        run(&mut cp, &mut c, "tc clk b:120");
        run(&mut cp, &mut c, "load a");
        run(&mut cp, &mut c, "group g -t c:clk -v a");

        let g = &c.groups[0];
        assert!(Rc::ptr_eq(&g.state.tempo, &c.tempo_cons[1]));
        assert!(Rc::ptr_eq(&g.voices[0].state.tempo, &c.tempo_cons[1]));
        assert_eq!(g.state.tempo.borrow().mode, TempoMode::Context);
    }

    // flags / process ids stay with their voice (B13)

    #[test]
    fn group_tempo_goes_to_the_right_voices() {
        sample_rate::set(48000);
        for _ in 0..10 {
            for perm in ["a,b,c", "b,c,a", "c,a,b", "c,b,a"] {
                let (mut cp, mut c) = setup(
                    1,
                    vec![track("a", 1, 16), track("b", 2, 16), track("c", 3, 16)],
                );
                run(&mut cp, &mut c, "load a");
                run(&mut cp, &mut c, "load b -t b:90");
                run(&mut cp, &mut c, "load c");
                run(&mut cp, &mut c, "seq a -t v -s 0");
                run(&mut cp, &mut c, &format!("group g -v {}", perm));

                let g = &c.groups[0];
                for v in &g.voices {
                    let shares = Rc::ptr_eq(&v.state.tempo, &g.state.tempo);
                    assert_eq!(shares, id(v) != 2, "voice {} in {}", id(v), perm);
                    if id(v) == 1 {
                        assert!(Rc::ptr_eq(&seq_tempo(&v.processes[0]), &g.state.tempo));
                    }
                }
            }
        }
    }

    // only voice-following processes move to the group tempo (B14)

    #[test]
    fn group_keeps_owned_seq_tempo() {
        sample_rate::set(48000);
        let (mut cp, mut c) = setup(1, vec![track("a", 1, 16)]);
        run(&mut cp, &mut c, "load a");
        // the voice-following seq goes first: until B22 gives each seq its
        // own mirror key, the second seq overwrites the first one's ProcRepr
        run(&mut cp, &mut c, "seq a -t v -s 0");
        run(&mut cp, &mut c, "seq a -t b:140 -s 0");
        let own = seq_tempo(&c.voices[0].processes[1]);
        run(&mut cp, &mut c, "group g -v a");

        let g = &c.groups[0];
        let a = &g.voices[0];
        assert!(Rc::ptr_eq(&a.state.tempo, &g.state.tempo));
        let after = seq_tempo(&a.processes[1]);
        assert!(Rc::ptr_eq(&after, &own));
        assert!(!Rc::ptr_eq(&after, &g.state.tempo));
        assert_eq!(after.borrow().mode, TempoMode::Process);
    }

    #[test]
    fn group_repoints_voice_following_seq() {
        sample_rate::set(48000);
        let (mut cp, mut c) = setup(1, vec![track("a", 1, 16)]);
        run(&mut cp, &mut c, "load a");
        run(&mut cp, &mut c, "seq a -t v -s 0");
        run(&mut cp, &mut c, "group g -v a");

        let g = &c.groups[0];
        assert!(Rc::ptr_eq(
            &seq_tempo(&g.voices[0].processes[0]),
            &g.state.tempo
        ));
    }

    // failed commands don't touch the mirror (B16)

    #[test]
    fn failed_group_keeps_mirror_and_engine_in_sync() {
        sample_rate::set(48000);
        let (mut cp, mut c) = setup(1, vec![track("a", 1, 16), track("b", 2, 16)]);
        run(&mut cp, &mut c, "load a");
        run(&mut cp, &mut c, "load b");
        assert!(cp.parse("group g -v a,nope").is_err());

        // b is still voices[1] on both sides
        run(&mut cp, &mut c, "velocity b 2");
        assert_eq!(c.voices.len(), 2);
        assert_eq!(c.voices[1].state.velocity, 2.0);
        assert_eq!(c.voices[0].state.velocity, 1.0);

        run(&mut cp, &mut c, "group g -v a");
        assert_eq!(id(&c.groups[0].voices[0]), 1);
        assert_eq!(id(&c.voices[0]), 2);
    }

    // duplicate names (B17)

    #[test]
    fn later_names_match_engine_positions_after_duplicates() {
        sample_rate::set(48000);
        let (mut cp, mut c) = setup(1, vec![track("a", 1, 16), track("b", 2, 16)]);
        run(&mut cp, &mut c, "tc x b:120");
        assert!(cp.parse("tc x b:90").is_err());
        run(&mut cp, &mut c, "tc y b:60");
        run(&mut cp, &mut c, "load a");
        run(&mut cp, &mut c, "load b");
        run(&mut cp, &mut c, "group g -v a");
        assert!(cp.parse("group g -v b").is_err());
        run(&mut cp, &mut c, "group h -t c:y -v b");

        assert_eq!(c.tempo_cons.len(), 2);
        assert_eq!(c.groups.len(), 2);
        assert!(Rc::ptr_eq(&c.groups[1].state.tempo, &c.tempo_cons[1]));
        assert_eq!(id(&c.groups[1].voices[0]), 2);
    }

    // leftovers

    #[test]
    fn empty_track_voice_doesnt_panic() {
        sample_rate::set(48000);
        let (mut cp, mut c) = setup(1, vec![AudioFile::new("e", "wav", 48000, 1, 16, vec![])]);
        run(&mut cp, &mut c, "load e");
        run(&mut cp, &mut c, "start -v e");
        assert!(render(&mut c, 1, 8).iter().all(|s| *s == 0));
    }

    #[test]
    fn group_stop_resets_voices() {
        let (mut cp, mut c) = grouped();
        run(&mut cp, &mut c, "start -g g");
        render(&mut c, 1, 5);
        assert_eq!(c.groups[0].voices[0].state.position, 5.0);

        run(&mut cp, &mut c, "stop -g g");
        for v in &c.groups[0].voices {
            assert!(!v.state.active);
            assert_eq!(v.state.position, 0.0);
        }
        // b's own tempo is stopped too
        let b = c.groups[0].voices[1].state.tempo.borrow();
        assert!(!b.active);
        assert_eq!(b.current, 0);
    }

    #[test]
    fn unload_group_voice() {
        let (mut cp, mut c) = grouped();
        run(&mut cp, &mut c, "unload g.a");
        assert_eq!(c.groups[0].voices.len(), 1);
        assert_eq!(c.groups[0].voices[0].samples[0], 2);
        assert_eq!(c.voices.len(), 1);

        // b moved to 0 in the mirror and the engine
        run(&mut cp, &mut c, "velocity g.b 2");
        assert_eq!(c.groups[0].voices[0].state.velocity, 2.0);

        assert!(cp.parse("unload g.a").is_err());
        run(&mut cp, &mut c, "unload c");
        assert!(c.voices.is_empty());
    }
}
