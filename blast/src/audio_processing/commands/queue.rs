//! The command queue to the audio thread (R1 replaces it with rtrb).

use super::Command;
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Single-producer, single-consumer ring of `Command`s.
pub struct CmdQueue {
    buf: Vec<UnsafeCell<Option<Command>>>,
    cap: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
}

unsafe impl Send for CmdQueue {}
unsafe impl Sync for CmdQueue {}

impl CmdQueue {
    /// A queue that holds `cap - 1` commands.
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

    /// Pushes `cmd`, or hands it back if the queue is full.
    // handing the Command back keeps the caller in charge of freeing it
    #[allow(clippy::result_large_err)]
    pub fn try_push(&self, cmd: Command) -> Result<(), Command> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);

        if (head + 1) % self.cap == tail {
            return Err(cmd);
        }

        unsafe {
            *self.buf[head].get() = Some(cmd);
        }

        self.head.store((head + 1) % self.cap, Ordering::Release);
        Ok(())
    }

    /// Only meaningful on the producer side: the consumer can only make
    /// room, so a queue that isn't full stays that way until the producer
    /// pushes.
    pub fn is_full(&self) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);

        (head + 1) % self.cap == tail
    }

    /// Pops the oldest command, if any.
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
