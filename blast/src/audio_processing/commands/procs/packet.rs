//! `packet`: seqs that share one pipe (RCV). A packet lives only in the
//! mirror; the engine sees its seqs, each carrying the packet's receivers.

use super::seq::{RecvSpec, SeqRecvArgs};
use crate::audio_processing::commands::lang::Code;
use crate::audio_processing::commands::reprs::{EngineState, PacketId, ProcId, ProcOwner};
use crate::audio_processing::commands::{CmdProcessor, Command};

/// A bundle of seqs with one pipe, in the mirror.
#[derive(Clone, Debug, PartialEq)]
pub struct PacketRepr {
    pub(crate) id: PacketId,
    pub(crate) name: String,
    /// Who holds it (`ProcOwner::Session` at the top level).
    pub(crate) owner: ProcOwner,
    /// Its seqs, in the order they were made.
    pub(crate) seqs: Vec<ProcId>,
    /// What its seqs do on each step (a pipe written once).
    pub(crate) recv: Vec<RecvSpec>,
}

impl EngineState {
    /// The packet `name` held by `owner`.
    pub(crate) fn packet_of(&self, owner: ProcOwner, name: &str) -> Option<PacketId> {
        self.packets
            .values()
            .find(|p| p.owner == owner && p.name == name)
            .map(|p| p.id)
    }

    /// The packets `owner` holds, by name.
    pub(crate) fn owner_packets(&self, owner: ProcOwner) -> Vec<&PacketRepr> {
        let mut found: Vec<&PacketRepr> =
            self.packets.values().filter(|p| p.owner == owner).collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found
    }

    /// Forgets the packets `owner` held, because `owner` is gone.
    pub(crate) fn drop_packets_of(&mut self, owner: ProcOwner) {
        self.packets.retain(|_, p| p.owner != owner);
    }

    /// Every seq (and its pipe) that names the voice or group `gone`.
    fn receivers_naming(&self, gone: ProcOwner) -> Vec<ProcId> {
        self.procs
            .values()
            .filter(|p| p.seq.recv.iter().any(|r| r.names() == Some(gone)))
            .map(|p| p.id)
            .collect()
    }

    /// Takes the receivers that name a removed voice or group (`gone_name`)
    /// out of every remaining pipe (a stale ID could name a new object), in
    /// the mirror. A pipe left with no receiver takes its seq (or its whole
    /// packet) with it, since nothing would trigger. Raises W102 for each
    /// seq or packet affected, and returns the commands for the engine: a
    /// `SeqRecv` for each seq that stays, the removal of each that goes.
    pub(crate) fn strip_receivers(&mut self, gone: ProcOwner, gone_name: &str) -> Vec<Command> {
        let mut cmds = Vec::new();
        // packets first: their seqs follow them
        let packets: Vec<PacketId> = self
            .packets
            .values()
            .filter(|p| p.recv.iter().any(|r| r.names() == Some(gone)))
            .map(|p| p.id)
            .collect();
        for id in packets {
            let Some(packet) = self.packets.get_mut(&id) else {
                continue;
            };
            packet.recv.retain(|r| r.names() != Some(gone));
            let (emptied, owner, name) =
                (packet.recv.is_empty(), packet.owner, packet.name.clone());
            let seqs = packet.seqs.clone();
            let path = self.owner_path(owner, &name);
            for seq in seqs {
                let Some(repr) = self.procs.get_mut(&seq) else {
                    continue;
                };
                repr.seq.recv.retain(|r| r.names() != Some(gone));
                let recv = repr.seq.recv.clone();
                if emptied {
                    cmds.push(self.remove_proc(seq));
                } else {
                    cmds.push(Command::SeqRecv(SeqRecvArgs { id: seq, recv }));
                }
            }
            if emptied {
                self.packets.remove(&id);
            }
            self.warn_receiver(&path, gone_name, emptied);
        }
        for id in self.receivers_naming(gone) {
            let Some(repr) = self.procs.get_mut(&id) else {
                continue;
            };
            repr.seq.recv.retain(|r| r.names() != Some(gone));
            let recv = repr.seq.recv.clone();
            let emptied = recv.is_empty();
            let path = self.proc_path(id).unwrap_or_default();
            if emptied {
                cmds.push(self.remove_proc(id));
            } else {
                cmds.push(Command::SeqRecv(SeqRecvArgs { id, recv }));
            }
            self.warn_receiver(&path, gone_name, emptied);
        }
        cmds
    }

    // W102 for `path`, whose pipe lost `gone`
    fn warn_receiver(&mut self, path: &str, gone: &str, emptied: bool) {
        let text = if emptied {
            format!("{path} triggered only {gone}; removed")
        } else {
            format!("{path} no longer triggers {gone}")
        };
        self.warnings.push((Code::ReceiverDropped, text));
    }
}

impl CmdProcessor {
    /// A new packet in the mirror; its seqs join it as they are made.
    pub(crate) fn make_packet(
        &mut self,
        owner: ProcOwner,
        name: &str,
        recv: Vec<RecvSpec>,
    ) -> PacketId {
        let es = &mut self.engine_state;
        let id = PacketId(es.next_packet);
        es.next_packet += 1;
        es.packets.insert(
            id,
            PacketRepr {
                id,
                name: name.to_string(),
                owner,
                seqs: Vec::new(),
                recv,
            },
        );
        id
    }
}
