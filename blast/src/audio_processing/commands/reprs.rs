//! The command thread's mirror of engine state.

use super::errors::{CmdErr, CmdResult};
use super::group::GroupRepr;
use super::tempo::TempoRepr;
use super::track::TrackRepr;
use super::voice::VoiceRepr;
use crate::file_parsing::decode_helpers::AudioFile;
use std::collections::HashMap;

/// An engine object, for terse commands like `start`; prefer the Reprs when
/// more is needed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Idx {
    Tempo(usize),
    Voice(usize),
    Process(usize),
    Group(usize),
    /// A Voice inside a Group: (Group idx, Voice idx in that Group); `Voice`
    /// is only for Voices in `Conductor.voices`.
    GroupVoice(usize, usize),
    // no Track variant: a TrackRepr is already just an index, and the few
    // Commands on Tracks are never ambiguous
}

/// Keeps track of all entities' states.
pub struct EngineState {
    pub(super) tracks: HashMap<String, TrackRepr>,
    pub(super) voices: HashMap<String, VoiceRepr>,
    pub(super) groups: HashMap<String, GroupRepr>,
    pub(super) tempo_cons: HashMap<String, TempoRepr>,
    #[allow(dead_code)] // not read yet; for validating routing
    out_channels: usize,
}

// "voice" or "group.voice"
fn split_voice(name: &str) -> CmdResult<(Option<&str>, &str)> {
    let mut parts = name.split('.');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(voice), None, _) => Ok((None, voice)),
        (Some(group), Some(voice), None) => Ok((Some(group), voice)),
        _ => Err(CmdErr::formatting(
            "Too many delimiters for format group.voice",
        )),
    }
}

fn no_voice(name: &str, group: Option<&str>) -> CmdErr {
    CmdErr::NoVoice {
        name: name.to_string(),
        group: group.map(str::to_string),
    }
}

fn no_item(ty: &str, name: &str) -> CmdErr {
    CmdErr::NoItem {
        ty: ty.to_string(),
        name: name.to_string(),
    }
}

impl EngineState {
    /// Track indices follow the order of `files`.
    pub fn new(files: Vec<AudioFile>, out_channels: usize) -> Self {
        let tracks = files
            .iter()
            .enumerate()
            .map(|(idx, af)| (af.file_name.clone(), TrackRepr::new(idx)))
            .collect();

        Self {
            tracks,
            out_channels,
            voices: HashMap::new(),
            groups: HashMap::new(),
            tempo_cons: HashMap::new(),
        }
    }

    pub(super) fn track(&self, name: &str) -> CmdResult<&TrackRepr> {
        self.tracks.get(name).ok_or_else(|| no_item("track", name))
    }

    /// A Voice named "voice" or "group.voice".
    pub(super) fn voice(&self, name: &str) -> CmdResult<&VoiceRepr> {
        match split_voice(name)? {
            (None, voice) => self.voices.get(voice).ok_or_else(|| no_voice(voice, None)),
            (Some(group), voice) => self
                .group(group)?
                .voices
                .get(voice)
                .ok_or_else(|| no_voice(voice, Some(group))),
        }
    }

    pub(super) fn voice_mut(&mut self, name: &str) -> CmdResult<&mut VoiceRepr> {
        match split_voice(name)? {
            (None, voice) => self
                .voices
                .get_mut(voice)
                .ok_or_else(|| no_voice(voice, None)),
            (Some(group), voice) => self
                .group_mut(group)?
                .voices
                .get_mut(voice)
                .ok_or_else(|| no_voice(voice, Some(group))),
        }
    }

    /// The engine Idx of a Voice: `Voice` or `GroupVoice`.
    pub(super) fn voice_idx(&self, name: &str) -> CmdResult<Idx> {
        let v_idx = self.voice(name)?.idx;
        match split_voice(name)? {
            (Some(group), _) => Ok(Idx::GroupVoice(self.group(group)?.idx, v_idx)),
            (None, _) => Ok(Idx::Voice(v_idx)),
        }
    }

    pub(super) fn group(&self, name: &str) -> CmdResult<&GroupRepr> {
        self.groups.get(name).ok_or_else(|| no_item("Group", name))
    }

    pub(super) fn group_mut(&mut self, name: &str) -> CmdResult<&mut GroupRepr> {
        self.groups
            .get_mut(name)
            .ok_or_else(|| no_item("Group", name))
    }

    /// A TempoContext.
    pub(super) fn context(&self, name: &str) -> CmdResult<&TempoRepr> {
        self.tempo_cons
            .get(name)
            .ok_or_else(|| no_item("TempoContext", name))
    }

    /// The object a `-v`/`-g`/`-t` flag names.
    pub(super) fn target(&self, flag: &str, name: &str) -> CmdResult<Idx> {
        match flag {
            "-v" | "--voice" => self.voice_idx(name),
            "-g" | "--group" => Ok(Idx::Group(self.group(name)?.idx)),
            "-t" | "--tempocontext" => Ok(Idx::Tempo(self.context(name)?.idx)),
            _ => Err(CmdErr::missing("type", "-v/-g/-t")),
        }
    }
}
