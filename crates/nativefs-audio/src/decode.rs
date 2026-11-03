//! File decoding with Symphonia. Every format comes out as interleaved `i32` samples, left-justified
//! (a 16-bit sample `s` becomes `s << 16`, a 24-bit one `s << 8`), so integer sources go through the
//! whole pipeline unchanged and can be written back to a 16 / 24 / 32-bit device bit for bit.

use std::fs::File;
use std::path::Path;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};

use crate::Error;

/// What the file contains (not what the device plays).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFormat {
    pub rate: u32,
    pub channels: u16,
    /// Valid bits per sample of an integer (lossless) source: 16, 24, 32. `None` for lossy / float
    /// sources, which have no "original samples" to preserve.
    pub bits: Option<u32>,
    /// Short codec name for the UI ("FLAC", "MP3", ...).
    pub codec: String,
}

pub struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    info: SourceFormat,
    total_frames: Option<u64>,
    /// Frames to drop from the next decoded packet (an accurate seek lands inside a packet).
    skip: u64,
    /// Position in frames of the next sample returned by `next`.
    pos: u64,
}

impl Decoder {
    pub fn open_path(path: &Path) -> Result<Self, Error> {
        let file = File::open(path).map_err(|e| Error::Io(e.to_string()))?;
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
        Self::open(Box::new(file), ext.as_deref())
    }

    /// `ext` is a hint for the prober (the file extension without the dot).
    pub fn open(source: Box<dyn MediaSource>, ext: Option<&str>) -> Result<Self, Error> {
        let mss = MediaSourceStream::new(source, Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = ext {
            hint.with_extension(ext);
        }
        let format = symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
            .map_err(|e| Error::Unsupported(e.to_string()))?;
        let track = format.default_track(TrackType::Audio).ok_or_else(|| Error::Unsupported("no audio track".into()))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| Error::Unsupported("no audio codec parameters".into()))?
            .clone();
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .map_err(|e| Error::Unsupported(e.to_string()))?;
        let rate = params.sample_rate.ok_or_else(|| Error::Unsupported("unknown sample rate".into()))?;
        let channels = params.channels.as_ref().map(|c| c.count() as u16).unwrap_or(2);
        let codec = symphonia::default::get_codecs()
            .get_audio_decoder(params.codec)
            .map(|d| d.codec.info.short_name.to_ascii_uppercase())
            .unwrap_or_else(|| "AUDIO".into());
        let lossless = matches!(codec.as_str(), "FLAC" | "ALAC" | "WAV") || codec.starts_with("PCM");
        let bits = if lossless { params.bits_per_sample.or(params.bits_per_coded_sample) } else { None };
        let info = SourceFormat { rate, channels, bits, codec };
        Ok(Self {
            track_id: track.id,
            time_base: track.time_base,
            total_frames: track.num_frames,
            format,
            decoder,
            info,
            skip: 0,
            pos: 0,
        })
    }

    pub fn info(&self) -> &SourceFormat {
        &self.info
    }

    pub fn total_frames(&self) -> Option<u64> {
        self.total_frames
    }

    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Appends the next block of interleaved samples to `out`. Returns `false` at the end of the file.
    pub fn next(&mut self, out: &mut Vec<i32>) -> Result<bool, Error> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(false),
                Err(SymError::ResetRequired) => return Err(Error::Unsupported("stream changed format".into())),
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(false),
                Err(e) => return Err(Error::Decode(e.to_string())),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let buf = match self.decoder.decode(&packet) {
                Ok(b) => b,
                // a damaged packet: skip it, like every player does
                Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => continue,
                Err(e) => return Err(Error::Decode(e.to_string())),
            };
            let ch = self.info.channels as usize;
            let frames = buf.frames();
            if frames == 0 {
                continue;
            }
            let start = out.len();
            out.resize(start + frames * ch, 0);
            buf.copy_to_slice_interleaved::<i32, _>(&mut out[start..]);
            let drop = (self.skip as usize).min(frames);
            if drop > 0 {
                out.drain(start..start + drop * ch);
                self.skip -= drop as u64;
            }
            self.pos += (frames - drop) as u64;
            if frames > drop {
                return Ok(true);
            }
        }
    }

    /// Seeks to `frame` exactly (the decoder output starts at that frame).
    pub fn seek(&mut self, frame: u64) -> Result<(), Error> {
        let secs = frame as f64 / self.info.rate as f64;
        let time = Time::try_from_secs_f64(secs).ok_or_else(|| Error::Decode("bad seek time".into()))?;
        let seeked = self
            .format
            .seek(SeekMode::Accurate, SeekTo::Time { time, track_id: Some(self.track_id) })
            .map_err(|e| Error::Decode(e.to_string()))?;
        self.decoder.reset();
        let actual = self.ts_to_frames(seeked.actual_ts);
        self.skip = frame.saturating_sub(actual);
        self.pos = actual + self.skip;
        Ok(())
    }

    fn ts_to_frames(&self, ts: Timestamp) -> u64 {
        // exact rational maths: going through nanoseconds rounded one frame low
        let ts = ts.get().max(0) as u128;
        match self.time_base {
            Some(tb) => (ts * tb.numer.get() as u128 * self.info.rate as u128 / tb.denom.get() as u128) as u64,
            None => ts as u64,
        }
    }
}
