use std::fmt;

/// Why a file couldn't be decoded.
#[derive(Debug)]
pub enum DecodeError {
    Io(std::io::Error),
    UnsupportedFormat(String),
    UnexpectedEof,
    InvalidData(String),
}

pub type DecodeResult<T> = Result<T, DecodeError>;

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Io(err) => write!(f, "I/O error: {err}"),
            DecodeError::UnsupportedFormat(what) => write!(f, "unsupported format: {what}"),
            DecodeError::UnexpectedEof => write!(f, "unexpected end of file"),
            DecodeError::InvalidData(what) => write!(f, "invalid data: {what}"),
        }
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DecodeError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for DecodeError {
    fn from(err: std::io::Error) -> Self {
        DecodeError::Io(err)
    }
}

impl From<symphonia::core::errors::Error> for DecodeError {
    fn from(err: symphonia::core::errors::Error) -> Self {
        use symphonia::core::errors::Error;
        match err {
            Error::IoError(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                DecodeError::UnexpectedEof
            }
            Error::IoError(e) => DecodeError::Io(e),
            Error::DecodeError(m) | Error::LimitError(m) => DecodeError::InvalidData(m.to_string()),
            Error::SeekError(_) => DecodeError::InvalidData("seek failed".to_string()),
            Error::Unsupported(m) => DecodeError::UnsupportedFormat(m.to_string()),
            Error::ResetRequired => {
                DecodeError::UnsupportedFormat("stream changes format mid-file".to_string())
            }
            // symphonia's Error is non-exhaustive
            other => DecodeError::InvalidData(other.to_string()),
        }
    }
}

/// A decoded track (README §3.1).
#[derive(Clone, Debug)]
pub struct AudioFile {
    pub file_name: String,
    /// Extension family: `wav`, `aiff`, `flac`, `mp3`, `m4a`, `aac`, ...
    pub format: String,
    pub sample_rate: u32,
    pub num_channels: u32,
    /// Bits per sample of the source; 0 for lossy codecs, where it doesn't apply.
    pub bits_per_sample: u32,
    /// WAVE channel mask (`dwChannelMask` bits), when the file states one.
    pub channel_layout: Option<u32>,
    /// Interleaved, full scale -1.0..1.0.
    pub samples: Vec<f32>,
}

impl AudioFile {
    /// A track with no channel layout; set `channel_layout` afterwards if known.
    pub fn new(
        file_name: &str,
        format: &str,
        sample_rate: u32,
        num_channels: u32,
        bits_per_sample: u32,
        samples: Vec<f32>,
    ) -> Self {
        Self {
            file_name: file_name.to_string(),
            format: format.to_string(),
            sample_rate,
            num_channels,
            bits_per_sample,
            channel_layout: None,
            samples,
        }
    }
}
