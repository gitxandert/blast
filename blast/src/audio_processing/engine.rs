use std::{
    rc::Rc, cell::RefCell,
    collections::{HashMap, hash_map::Entry},
};

use alsa_sys::*;

use crate::file_parsing::decode_helpers::{
    DecodeResult, DecodeError, AudioFile,
};
use crate::audio_processing::{
    commands::*, // too many to list
    processes::*, // this will be ditto
    blast_rand::{
        X128P, fast_seed
    },
    blast_time::{
        sample_rate,
        blast_time::{
            clock, TempoMode, TempoUnit, TempoState
        }
    },
};

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

    pub fn coordinate(&mut self, areas_ptr: *const snd_pcm_channel_area_t, offset: snd_pcm_uframes_t, frames: snd_pcm_uframes_t) {
        unsafe {
            let areas = std::slice::from_raw_parts(areas_ptr, self.out_channels);

            for f in 0..frames {
                for ch in 0..self.out_channels {
                    let a = &areas[ch];
                    let base = a.addr as *mut u8;

                    // ALSA channel area addressing
                    let bit_offset = a.first as isize + (offset + f) as isize * a.step as isize;
                    let byte_offset = bit_offset / 8;

                    let sample_ptr = base.offset(byte_offset) as *mut i16;
            
                    unsafe {
                        *sample_ptr = 0;
                    }

                    for voice in &mut self.voices {
                        if voice.state.active {
                            voice.process(sample_ptr, f, ch);
                        }
                    }

                    for group in &mut self.groups {
                        if group.state.active {
                            group.process(sample_ptr, f, ch);
                        }
                    }
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
            Command::Quit(_) => {
                unsafe {
                    libc::raise(libc::SIGTERM);
                }
            }
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
        self.voices.remove(args.idx);
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
            Idx::GroupVoice(g, v) if !args.tempo.owned
                && (args.tempo.mode == TempoMode::Voice || args.tempo.mode == TempoMode::TBD) => {
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
        };
        
        match args.idx {
            Idx::Voice(v) => {
                let voice: &mut Voice = self.voices.get_mut(v).unwrap();
                voice.processes.push(Process::Seq(Seq { state }));
                if args.tempo.mode == TempoMode::Process {
                    voice.proc_tempi.push(tempo);
                }
            }
            Idx::GroupVoice(g, v) => {
                let voice: &mut Voice = self.groups[g].voices.get_mut(v).unwrap();
                voice.processes.push(Process::Seq(Seq { state }));
                if args.tempo.mode == TempoMode::Process {
                    voice.proc_tempi.push(tempo);
                }
            }
            Idx::Group(g) => {
                let group: &mut Group = self.groups.get_mut(g).unwrap();
                group.processes.push(Process::Seq(Seq { state }));
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

// instead of samples, Voices have a Function,
// which are either some type of Wave or a collection of samples
enum Function {
    Track { samples: Vec<i16> },
    Wave { form: WaveForm }
}

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
            end: af.samples.len() / af.num_channels as usize - 1,
            velocity: 1.0,
            gain: 1.0,
            tempo: tempo_state
        };

        Self {
            samples: af.samples.clone(),
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

        let mut ts = state.tempo.borrow_mut();
        if ts.mode == TempoMode::Voice {
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

    fn process(&mut self, acc: *mut i16, frame: u64, mut ch: usize) {
        if !self.state.active { return; }

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

        let idx = state.position as usize;
        if idx >= state.end || idx < 0 {
            return;
        }

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
        let mut sample = 0f32;
        let s0 = self.samples[(idx * self.channels) + (ch % self.channels)] as f32;
        if state.velocity != 1.0 {
            let frac = state.position.fract();
            let s1 = self.samples[((idx + 1) * self.channels) + (ch % self.channels)] as f32;
            sample = s0 * (1.0 - frac) + s1 * frac;
        } else {
            sample = s0;
        }

        unsafe {
            *acc += (sample * state.gain) as i16;
        }

        // advance
        if ch == self.channels - 1 {
            state.position += state.velocity;
        }
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

        for mut voice in &mut self.voices {
            voice.state.active = false;
        }
                
        let mut ts = self.state.tempo.borrow_mut();
        if ts.mode == TempoMode::Group {
            ts.active = false;
            ts.reset();
        }
    }

    fn process(&mut self, acc: *mut i16, frame: u64, mut ch: usize) {
        if !self.state.active { return; }

        // processing
        for v in &mut self.voices {
            v.process(acc, frame, ch);
        }

        let mut ts = self.state.tempo.borrow_mut();
        if ts.mode == TempoMode::Group {
            ts.update(1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_processing::test_util::{setup, run, track, render};

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
        let (mut cp, mut c) = setup(1, vec![
            track("a", 1, 16), track("b", 2, 16), track("c", 3, 16), track("d", 4, 16),
        ]);
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
        assert!(Rc::ptr_eq(&c.voices[1].state.tempo, &c.groups[0].state.tempo));
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
        assert!(Rc::ptr_eq(&seq_tempo(&c.voices[0].processes[0]), &c.tempo_cons[0]));
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
            let (mut cp, mut c) = setup(1, vec![
                track("a", 1, 16), track("b", 2, 16), track("c", 3, 16),
            ]);
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
        let (mut cp, mut c) = setup(1, vec![
            track("a", 1, 16), track("b", 2, 16), track("c", 3, 16),
        ]);
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
}
