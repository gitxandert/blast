//! The tap's reader: cuts the stereo tap into chunks and broadcasts them to
//! listening browsers as binary WebSocket messages.
//!
//! Message layout (little-endian), so the browser needs no other state:
//!
//! | bytes | field                              |
//! |-------|------------------------------------|
//! | 0     | `1` (audio)                        |
//! | 1     | sample format: `0` i16, `1` f32    |
//! | 2..4  | channels (u16, always 2 for now)   |
//! | 4..8  | sample rate (u32)                  |
//! | 8..12 | sequence number (u32, wraps)       |
//! | 12..  | interleaved samples                |

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use axum::body::Bytes;
use tokio::sync::broadcast;

use super::output::Shared;

pub const HEADER: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireFormat {
    I16,
    F32,
}

impl WireFormat {
    pub fn parse(s: &str) -> WireFormat {
        if s == "f32" {
            WireFormat::F32
        } else {
            WireFormat::I16
        }
    }
}

/// Encodes one chunk of interleaved stereo.
pub fn encode(samples: &[f32], rate: u32, seq: u32, format: WireFormat) -> Bytes {
    let width = match format {
        WireFormat::I16 => 2,
        WireFormat::F32 => 4,
    };
    let mut out = Vec::with_capacity(HEADER + samples.len() * width);
    out.push(1);
    out.push(match format {
        WireFormat::I16 => 0,
        WireFormat::F32 => 1,
    });
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    match format {
        WireFormat::I16 => {
            for &x in samples {
                let v = (x.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        WireFormat::F32 => {
            for &x in samples {
                out.extend_from_slice(&x.to_le_bytes());
            }
        }
    }
    Bytes::from(out)
}

/// Starts the reader thread.
pub fn start(
    shared: Arc<Shared>,
    mut tap: rtrb::Consumer<f32>,
    tx: broadcast::Sender<Bytes>,
    chunk_ms: u32,
    format: WireFormat,
) {
    thread::Builder::new()
        .name("blast-server-tap".into())
        .spawn(move || {
            let mut seq: u32 = 0;
            let mut buf: Vec<f32> = Vec::new();
            loop {
                thread::sleep(Duration::from_millis(3));
                let rate = shared.rate.load(Ordering::Relaxed);
                let chunk = (rate as usize * chunk_ms as usize / 1000).max(64) * 2;
                while tap.slots() >= chunk {
                    let Ok(read) = tap.read_chunk(chunk) else {
                        break;
                    };
                    let (a, b) = read.as_slices();
                    let listening =
                        tx.receiver_count() > 0 && shared.tap_on.load(Ordering::Relaxed);
                    if listening {
                        buf.clear();
                        buf.extend_from_slice(a);
                        buf.extend_from_slice(b);
                    }
                    read.commit_all();
                    if listening {
                        let _ = tx.send(encode(&buf, rate, seq, format));
                        seq = seq.wrapping_add(1);
                    }
                }
            }
        })
        .expect("spawn tap thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_and_samples() {
        let b = encode(&[0.0, 1.0, -1.0, 0.5], 48_000, 7, WireFormat::I16);
        assert_eq!(b.len(), HEADER + 8);
        assert_eq!(b[0], 1);
        assert_eq!(b[1], 0);
        assert_eq!(u16::from_le_bytes([b[2], b[3]]), 2);
        assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()), 48_000);
        assert_eq!(u32::from_le_bytes(b[8..12].try_into().unwrap()), 7);
        assert_eq!(i16::from_le_bytes([b[14], b[15]]), 32767);
        assert_eq!(i16::from_le_bytes([b[16], b[17]]), -32767);
        let f = encode(&[0.25, -0.25], 44_100, 0, WireFormat::F32);
        assert_eq!(f[1], 1);
        assert_eq!(f32::from_le_bytes(f[12..16].try_into().unwrap()), 0.25);
    }
}
