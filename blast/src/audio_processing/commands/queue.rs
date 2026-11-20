//! The command queue to the audio thread: an rtrb pair (README §3.4).

use super::Command;

/// The REPL end of the command queue.
pub struct CmdSender(rtrb::Producer<Command>);

/// The audio-thread end of the command queue.
pub struct CmdReceiver(rtrb::Consumer<Command>);

/// A queue that holds `cap` commands.
pub fn cmd_queue(cap: usize) -> (CmdSender, CmdReceiver) {
    let (tx, rx) = rtrb::RingBuffer::new(cap);
    (CmdSender(tx), CmdReceiver(rx))
}

impl CmdSender {
    /// Whether the queue is full. The consumer can only make room, so a
    /// queue that isn't full stays that way until this end pushes.
    pub fn is_full(&self) -> bool {
        self.0.is_full()
    }

    /// Pushes `cmd`, or hands it back if the queue is full.
    // handing the Command back keeps the caller in charge of freeing it
    #[allow(clippy::result_large_err)]
    pub fn try_push(&mut self, cmd: Command) -> Result<(), Command> {
        self.0.push(cmd).map_err(|rtrb::PushError::Full(c)| c)
    }
}

impl CmdReceiver {
    /// Pops the oldest command, if any.
    pub fn try_pop(&mut self) -> Option<Command> {
        self.0.pop().ok()
    }
}
