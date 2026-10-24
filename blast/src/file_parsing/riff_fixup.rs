//! Byte-level repairs for WAV and AIFF files that symphonia would reject or
//! misread but that are common in the wild (REQ-031). Everything here is
//! bounds-checked: file data is never indexed directly.

/// A chunk header found while walking a RIFF or FORM container.
struct Chunk {
    id: [u8; 4],
    /// Offset of the 8-byte header.
    at: usize,
    /// Declared body size.
    size: usize,
}

impl Chunk {
    fn body(&self) -> usize {
        self.at + 8
    }

    /// End of the body plus its pad byte, as declared (may run past the file).
    fn end(&self) -> usize {
        self.body()
            .saturating_add(self.size)
            .saturating_add(self.size & 1)
    }
}

fn read_u16(bytes: &[u8], at: usize, big_endian: bool) -> Option<u16> {
    let b: [u8; 2] = bytes.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(if big_endian {
        u16::from_be_bytes(b)
    } else {
        u16::from_le_bytes(b)
    })
}

fn read_u32(bytes: &[u8], at: usize, big_endian: bool) -> Option<u32> {
    let b: [u8; 4] = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(if big_endian {
        u32::from_be_bytes(b)
    } else {
        u32::from_le_bytes(b)
    })
}

fn write_u32(bytes: &mut [u8], at: usize, value: u32, big_endian: bool) {
    let b = if big_endian {
        value.to_be_bytes()
    } else {
        value.to_le_bytes()
    };
    if let Some(dst) = at.checked_add(4).and_then(|end| bytes.get_mut(at..end)) {
        dst.copy_from_slice(&b);
    }
}

// every chunk header from offset 12 on, ignoring the container size;
// stops at the first chunk that runs past the end of the file
fn walk(bytes: &[u8], big_endian: bool) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut at = 12usize;
    while let Some(id) = at
        .checked_add(4)
        .and_then(|end| bytes.get(at..end))
        .and_then(|id| <[u8; 4]>::try_from(id).ok())
    {
        let Some(size) = read_u32(bytes, at + 4, big_endian) else {
            break;
        };
        let chunk = Chunk {
            id,
            at,
            size: size as usize,
        };
        let end = chunk.end();
        chunks.push(chunk);
        if end > bytes.len() {
            break;
        }
        at = end;
    }
    chunks
}

// raise the container size to cover the chunks that matter, never past the
// file and never down, so bytes after them stay outside the container
fn cover(bytes: &mut [u8], needed_end: usize, big_endian: bool) {
    let Some(declared) = read_u32(bytes, 4, big_endian) else {
        return;
    };
    let needed_end = needed_end.min(bytes.len());
    if (declared as usize).saturating_add(8) < needed_end {
        let size = u32::try_from(needed_end - 8).unwrap_or(u32::MAX);
        write_u32(bytes, 4, size, big_endian);
    }
}

/// Repairs `bytes` for symphonia. Returns the bytes to decode and an optional
/// frame limit from the header (`fact` for non-PCM WAV, COMM for AIFF), which
/// the caller only ever uses to shorten the output.
pub(super) fn repair(bytes: Vec<u8>) -> (Vec<u8>, Option<u64>) {
    let magic = bytes.get(0..4);
    let kind = bytes.get(8..12);
    match (magic, kind) {
        (Some(b"RIFF"), Some(b"WAVE")) => repair_wav(bytes),
        (Some(b"FORM"), Some(b"AIFF" | b"AIFC")) => repair_aiff(bytes),
        _ => (bytes, None),
    }
}

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

fn repair_wav(mut bytes: Vec<u8>) -> (Vec<u8>, Option<u64>) {
    let chunks = walk(&bytes, false);
    let find = |id: &[u8; 4]| chunks.iter().position(|c| &c.id == id);
    let (fmt, data, fact) = (find(b"fmt "), find(b"data"), find(b"fact"));

    // a data chunk that runs past the file (streamed or unfinalised) ends at it
    if let Some(d) = data.and_then(|i| chunks.get(i)) {
        let present = bytes.len().saturating_sub(d.body());
        if d.size > present {
            let size = u32::try_from(present).unwrap_or(u32::MAX);
            write_u32(&mut bytes, d.at + 4, size, false);
        }
    }

    let needed_end = [fmt, data]
        .iter()
        .flatten()
        .filter_map(|&i| chunks.get(i))
        .map(Chunk::end)
        .max();
    if let Some(end) = needed_end {
        cover(&mut bytes, end, false);
    }

    let limit = fmt.and_then(|i| chunks.get(i)).and_then(|f| {
        let mut tag = read_u16(&bytes, f.body(), false)?;
        if tag == WAVE_FORMAT_EXTENSIBLE {
            // the sub-format GUID starts with the real format tag
            tag = read_u16(&bytes, f.body() + 24, false)?;
        }
        if tag == WAVE_FORMAT_PCM || tag == WAVE_FORMAT_IEEE_FLOAT {
            return None;
        }
        let fact = chunks.get(fact?)?;
        read_u32(&bytes, fact.body(), false).map(u64::from)
    });

    // symphonia stops at `data`, so `fmt ` must come first
    if let (Some(f), Some(d)) = (fmt, data)
        && f > d
        && let Some(f) = chunks.get(f)
    {
        let fmt_end = f.end().min(bytes.len());
        if let (Some(head), Some(fmt_chunk), Some(between), Some(rest)) = (
            bytes.get(..12),
            bytes.get(f.at..fmt_end),
            bytes.get(12..f.at),
            bytes.get(fmt_end..),
        ) {
            let reordered = [head, fmt_chunk, between, rest].concat();
            return (reordered, limit);
        }
    }

    (bytes, limit)
}

fn repair_aiff(mut bytes: Vec<u8>) -> (Vec<u8>, Option<u64>) {
    let chunks = walk(&bytes, true);
    let find = |id: &[u8; 4]| chunks.iter().find(|c| &c.id == id);
    let (comm, ssnd) = (find(b"COMM"), find(b"SSND"));

    let needed_end = [comm, ssnd].iter().flatten().map(|c| c.end()).max();
    if let Some(end) = needed_end {
        cover(&mut bytes, end, true);
    }

    // COMM: channels (u16), then the frame count (u32)
    let limit = comm.and_then(|c| read_u32(&bytes, c.body() + 2, true).map(u64::from));
    (bytes, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_parsing::test_bytes::*;

    #[test]
    fn leaves_other_files_alone() {
        let bytes = b"fLaC\0\0\0\x22 and more".to_vec();
        assert_eq!(repair(bytes.clone()), (bytes, None));
        assert_eq!(repair(Vec::new()), (Vec::new(), None));
    }

    #[test]
    fn valid_pcm_wav_is_unchanged() {
        let bytes = riff(&[
            chunk_le(b"fmt ", &fmt_body(1, 2, 48000, 16)),
            chunk_le(b"data", &i16_le(&[1, 2, 3, 4])),
        ]);
        assert_eq!(repair(bytes.clone()), (bytes, None));
    }

    #[test]
    fn fixes_sizes_in_place() {
        let mut bytes = riff(&[
            chunk_le(b"fmt ", &fmt_body(1, 1, 48000, 16)),
            chunk_le(b"data", &i16_le(&[1, 2, 3, 4])),
        ]);
        let len = bytes.len();
        bytes[4..8].copy_from_slice(&20u32.to_le_bytes());
        bytes[len - 12..len - 8].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        let (fixed, limit) = repair(bytes);
        assert_eq!(limit, None);
        assert_eq!(read_u32(&fixed, 4, false), Some(len as u32 - 8));
        assert_eq!(read_u32(&fixed, len - 12, false), Some(8));
    }

    #[test]
    fn never_shrinks_riff_size() {
        let mut bytes = riff(&[
            chunk_le(b"fmt ", &fmt_body(1, 1, 48000, 16)),
            chunk_le(b"data", &i16_le(&[1, 2])),
        ]);
        bytes[4..8].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        let (fixed, _) = repair(bytes);
        assert_eq!(read_u32(&fixed, 4, false), Some(0xFFFF_FFFF));
    }

    #[test]
    fn moves_fmt_before_data() {
        let fmt = chunk_le(b"fmt ", &fmt_body(1, 1, 48000, 16));
        let data = chunk_le(b"data", &i16_le(&[1, 2]));
        let list = chunk_le(b"LIST", &[0; 3]);
        let (fixed, _) = repair(riff(&[data.clone(), list.clone(), fmt.clone()]));
        assert_eq!(fixed, riff(&[fmt, data, list]));
    }

    #[test]
    fn fact_limits_only_non_pcm() {
        let fact = chunk_le(b"fact", &5u32.to_le_bytes());
        let data = chunk_le(b"data", &[0; 64]);
        let pcm = riff(&[
            chunk_le(b"fmt ", &fmt_body(1, 1, 8000, 16)),
            fact.clone(),
            data.clone(),
        ]);
        assert_eq!(repair(pcm).1, None);
        let adpcm = riff(&[chunk_le(b"fmt ", &fmt_body(0x11, 1, 8000, 4)), fact, data]);
        assert_eq!(repair(adpcm).1, Some(5));
    }

    #[test]
    fn aiff_limit_is_comm_frames() {
        let bytes = form(
            b"AIFF",
            &[
                chunk_be(b"COMM", &comm_body(2, 2, 16, 44100)),
                chunk_be(b"SSND", &ssnd_body(0, &[0; 12])),
            ],
        );
        assert_eq!(repair(bytes.clone()), (bytes, Some(2)));
    }

    #[test]
    fn never_panics_on_prefixes() {
        let bytes = riff(&[
            chunk_le(b"data", &i16_le(&[1, 2, 3])),
            chunk_le(b"fmt ", &fmt_body(0x11, 1, 8000, 4)),
            chunk_le(b"fact", &3u32.to_le_bytes()),
        ]);
        for n in 0..=bytes.len() {
            let _ = repair(bytes[..n].to_vec());
        }
    }
}
