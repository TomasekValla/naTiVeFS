//! naTiVeFS audio: decoding (Symphonia), bit-perfect output (WASAPI exclusive on Windows, ALSA `hw:` on
//! Linux, CoreAudio on macOS) and the DSP chain of the TVFS web player.

pub mod analysis;
pub mod decode;
pub mod dsp;
pub mod engine;
pub mod output;
pub mod remote;
pub mod ring;
pub mod vis;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Io(String),
    Unsupported(String),
    Decode(String),
    Device(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(m) => write!(f, "could not read the file: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported audio: {m}"),
            Error::Decode(m) => write!(f, "decoding failed: {m}"),
            Error::Device(m) => write!(f, "audio device: {m}"),
        }
    }
}

impl std::error::Error for Error {}
