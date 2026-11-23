//! A safe wrapper over Opus (libopus 1.3.1 translated to Rust — no C compiler
//! or external libraries, builds the same on Linux and Windows).

use std::fmt;

#[allow(clippy::unsafe_removed_from_name)]
use unsafe_libopus as opus;
use unsafe_libopus::varargs::{VarArg, VarArgs};

use crate::{FRAME, SAMPLE_RATE};

/// Default bitrate: clear wideband speech at ~4 KB/s.
pub const DEFAULT_BITRATE: i32 = 32_000;

/// Buffer for one encoded frame.
pub const MAX_PACKET: usize = 1275;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecError(pub i32);

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.0 {
            opus::OPUS_BAD_ARG => "bad argument",
            opus::OPUS_BUFFER_TOO_SMALL => "buffer too small",
            opus::OPUS_INTERNAL_ERROR => "internal error",
            opus::OPUS_INVALID_PACKET => "corrupt packet",
            opus::OPUS_UNIMPLEMENTED => "not supported",
            opus::OPUS_INVALID_STATE => "invalid state",
            opus::OPUS_ALLOC_FAIL => "out of memory",
            _ => "unknown error",
        };
        write!(f, "opus: {what} ({})", self.0)
    }
}

impl std::error::Error for CodecError {}

pub struct Encoder {
    st: *mut opus::OpusEncoder,
}

// SAFETY: the codec state is plain heap memory not tied to a thread; access
// only through &mut self.
unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new(bitrate: i32) -> Result<Self, CodecError> {
        let mut err = 0;
        // SAFETY: the arguments are valid; the pointer is checked below.
        let st = unsafe {
            opus::opus_encoder_create(SAMPLE_RATE as i32, 1, opus::OPUS_APPLICATION_VOIP, &mut err)
        };
        if err != opus::OPUS_OK || st.is_null() {
            return Err(CodecError(err));
        }
        let mut enc = Self { st };
        enc.ctl(opus::OPUS_SET_BITRATE_REQUEST, bitrate)?;
        enc.ctl(opus::OPUS_SET_SIGNAL_REQUEST, opus::OPUS_SIGNAL_VOICE)?;
        enc.ctl(opus::OPUS_SET_COMPLEXITY_REQUEST, 8)?;
        Ok(enc)
    }

    fn ctl(&mut self, request: i32, value: i32) -> Result<(), CodecError> {
        // SAFETY: self.st lives as long as self.
        let r = unsafe {
            opus::opus_encoder_ctl_impl(self.st, request, VarArgs::new(vec![VarArg::I32(value)]))
        };
        if r == opus::OPUS_OK { Ok(()) } else { Err(CodecError(r)) }
    }

    pub fn set_bitrate(&mut self, bitrate: i32) -> Result<(), CodecError> {
        self.ctl(opus::OPUS_SET_BITRATE_REQUEST, bitrate.clamp(8_000, 128_000))
    }

    /// Encode one frame (20 ms, −1..1). Returns the packet length in `out`.
    pub fn encode(&mut self, pcm: &[f32; FRAME], out: &mut [u8; MAX_PACKET]) -> Result<usize, CodecError> {
        // SAFETY: the buffer lengths match the sizes passed.
        let n = unsafe {
            opus::opus_encode_float(self.st, pcm.as_ptr(), FRAME as i32, out.as_mut_ptr(), out.len() as i32)
        };
        if n < 0 { Err(CodecError(n)) } else { Ok(n as usize) }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: the pointer came from opus_encoder_create and is freed once.
        unsafe { opus::opus_encoder_destroy(self.st) }
    }
}

pub struct Decoder {
    st: *mut opus::OpusDecoder,
}

// SAFETY: same as for Encoder.
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new() -> Result<Self, CodecError> {
        let mut err = 0;
        // SAFETY: the arguments are valid; the pointer is checked below.
        let st = unsafe { opus::opus_decoder_create(SAMPLE_RATE as i32, 1, &mut err) };
        if err != opus::OPUS_OK || st.is_null() {
            return Err(CodecError(err));
        }
        Ok(Self { st })
    }

    /// Decode a packet into one frame. Packets of another duration are refused.
    pub fn decode(&mut self, packet: &[u8], out: &mut [f32; FRAME]) -> Result<(), CodecError> {
        if packet.is_empty() || packet.len() > MAX_PACKET {
            return Err(CodecError(opus::OPUS_INVALID_PACKET));
        }
        // SAFETY: packet is not empty, its length is passed correctly.
        let samples = unsafe {
            opus::opus_packet_get_nb_samples(packet.as_ptr(), packet.len() as i32, SAMPLE_RATE as i32)
        };
        if samples != FRAME as i32 {
            return Err(CodecError(opus::OPUS_INVALID_PACKET));
        }
        // SAFETY: out holds FRAME samples.
        let n = unsafe {
            opus::opus_decode_float(self.st, packet.as_ptr(), packet.len() as i32, out.as_mut_ptr(), FRAME as i32, 0)
        };
        if n == FRAME as i32 { Ok(()) } else { Err(CodecError(if n < 0 { n } else { opus::OPUS_INTERNAL_ERROR })) }
    }

    /// A packet did not arrive: the codec fills in the sound (packet loss concealment).
    pub fn conceal(&mut self, out: &mut [f32; FRAME]) -> Result<(), CodecError> {
        // SAFETY: a null pointer with length 0 is the documented way to ask for PLC.
        let n = unsafe {
            opus::opus_decode_float(self.st, std::ptr::null(), 0, out.as_mut_ptr(), FRAME as i32, 0)
        };
        if n == FRAME as i32 { Ok(()) } else { Err(CodecError(if n < 0 { n } else { opus::OPUS_INTERNAL_ERROR })) }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: the pointer came from opus_decoder_create and is freed once.
        unsafe { opus::opus_decoder_destroy(self.st) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::rms;
    use std::f32::consts::PI;

    fn tone_frame(n: usize, freq: f32) -> [f32; FRAME] {
        let mut f = [0.0; FRAME];
        for (i, s) in f.iter_mut().enumerate() {
            *s = 0.4 * (2.0 * PI * freq * (n * FRAME + i) as f32 / SAMPLE_RATE as f32).sin();
        }
        f
    }

    #[test]
    fn roundtrip_preserves_level_and_is_compact() {
        let mut enc = Encoder::new(DEFAULT_BITRATE).unwrap();
        let mut dec = Decoder::new().unwrap();
        let mut packet = [0u8; MAX_PACKET];
        let mut out = [0.0; FRAME];
        let mut bytes = 0;
        let mut level = 0.0;
        for n in 0..50 {
            let len = enc.encode(&tone_frame(n, 440.0), &mut packet).unwrap();
            assert!(len > 0 && len < 400);
            bytes += len;
            dec.decode(&packet[..len], &mut out).unwrap();
            if n >= 10 {
                level += rms(&out);
            }
        }
        let level = level / 40.0;
        assert!((level - 0.4 / 2f32.sqrt()).abs() < 0.05, "the level is kept: {level}");
        // 50 frames = 1 second; 32 kbit/s ≈ 4000 bytes
        assert!(bytes < 6_000, "a second of speech took {bytes} bytes");
    }

    #[test]
    fn concealment_fills_a_gap_without_error() {
        let mut enc = Encoder::new(DEFAULT_BITRATE).unwrap();
        let mut dec = Decoder::new().unwrap();
        let mut packet = [0u8; MAX_PACKET];
        let mut out = [0.0; FRAME];
        for n in 0..10 {
            let len = enc.encode(&tone_frame(n, 300.0), &mut packet).unwrap();
            dec.decode(&packet[..len], &mut out).unwrap();
        }
        dec.conceal(&mut out).unwrap();
        assert!(rms(&out) > 0.01, "PLC carries the sound on instead of cutting to silence");
    }

    #[test]
    fn garbage_packets_are_rejected() {
        let mut dec = Decoder::new().unwrap();
        let mut out = [0.0; FRAME];
        assert!(dec.decode(&[], &mut out).is_err());
        assert!(dec.decode(&[0u8; MAX_PACKET + 1], &mut out).is_err());
        // valid Opus, but a 10 ms frame instead of 20 (TOC 0x40 = SILK WB 10 ms)
        assert!(dec.decode(&[0x40, 0, 0, 0], &mut out).is_err());
    }
}
