//! Wave commands (WAVa).

use crate::audio_processing::commands::{WaveArgs, WaveOp};
use crate::audio_processing::engine::{Conductor, Garbage};

impl Conductor {
    /// Applies an op to one wave of a voice. Nothing allocates: a voice's
    /// waves are a fixed array.
    pub(crate) fn wave(&mut self, args: WaveArgs) {
        let Some(voice) = self.voices.get_mut(args.voice.index()) else {
            // a waiting line outlived the voice; its table leaves as garbage
            if let WaveOp::Table(table) = args.op {
                self.retire(Garbage::WaveTable(table));
            }
            return;
        };
        let (id, active) = (args.id, voice.state.active);
        let waves = &mut voice.waves;
        match args.op {
            WaveOp::Insert {
                shape,
                params,
                retrig,
            } => waves.insert(id, shape, params, retrig),
            WaveOp::Set {
                param,
                value,
                frames,
            } => {
                // a voice that isn't running can't glide (nothing advances it)
                let frames = if active { frames } else { 0 };
                let used = waves.set(id, param, value, frames);
                self.note_glide(used);
            }
            WaveOp::Shape(shape) => waves.set_shape(id, shape),
            WaveOp::Retrig(on) => waves.set_retrig(id, on),
            WaveOp::Bypass { on, clear } => waves.bypass(id, on, clear),
            WaveOp::Fm(modulators) => waves.set_fm(id, modulators),
            WaveOp::Out(out) => waves.set_out(id, out),
            WaveOp::RatioMode(on) => waves.set_ratio_mode(id, on),
            WaveOp::Remove => {
                if let Some(old) = waves.remove(id) {
                    self.retire(Garbage::WaveTable(old));
                }
            }
            WaveOp::Table(table) => match waves.set_table(id, table) {
                Ok(Some(old)) => self.retire(Garbage::WaveTable(old)),
                Ok(None) => {}
                // the wave is gone: the new table leaves unused
                Err(table) => self.retire(Garbage::WaveTable(table)),
            },
        }
    }
}
