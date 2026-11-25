//! The binary video frame. Each frame travels between peers on a QUIC stream
//! of its own, so a late frame never holds up a newer one.
//!
//! ```text
//! [0]      version (1)
//! [1]      source: 0 — a camera, 1 — a screen
//! [2..6]   sequence number, u32 BE
//! [6..8]   width, u16 BE
//! [8..10]  height, u16 BE
//! [10..]   a JPEG picture of that size
//! ```

use crate::VideoSource;

pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 10;
/// Biggest frame accepted, header included.
pub const MAX_FRAME: usize = 768 << 10;
/// Biggest picture accepted.
pub const MAX_WIDTH: u16 = 1920;
pub const MAX_HEIGHT: u16 = 1200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    pub source: VideoSource,
    pub seq: u32,
    pub width: u16,
    pub height: u16,
    pub jpeg: &'a [u8],
}

pub fn encode(source: VideoSource, seq: u32, width: u16, height: u16, jpeg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + jpeg.len());
    out.push(VERSION);
    out.push(match source {
        VideoSource::Camera => 0,
        VideoSource::Screen => 1,
    });
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(jpeg);
    out
}

/// Parse a frame; `None` — another version, a bad size, or not a JPEG.
pub fn parse(buf: &[u8]) -> Option<Frame<'_>> {
    if buf.len() < HEADER_LEN + 4 || buf.len() > MAX_FRAME || buf[0] != VERSION {
        return None;
    }
    let source = match buf[1] {
        0 => VideoSource::Camera,
        1 => VideoSource::Screen,
        _ => return None,
    };
    let width = u16::from_be_bytes([buf[6], buf[7]]);
    let height = u16::from_be_bytes([buf[8], buf[9]]);
    if !(1..=MAX_WIDTH).contains(&width) || !(1..=MAX_HEIGHT).contains(&height) {
        return None;
    }
    let jpeg = &buf[HEADER_LEN..];
    if jpeg[..2] != [0xff, 0xd8] {
        return None;
    }
    Some(Frame { source, seq: u32::from_be_bytes([buf[2], buf[3], buf[4], buf[5]]), width, height, jpeg })
}

#[cfg(test)]
mod tests {
    use super::*;

    const JPEG: [u8; 6] = [0xff, 0xd8, 1, 2, 0xff, 0xd9];

    #[test]
    fn roundtrip() {
        let buf = encode(VideoSource::Screen, 70_000, 640, 360, &JPEG);
        let f = parse(&buf).unwrap();
        assert_eq!(f, Frame { source: VideoSource::Screen, seq: 70_000, width: 640, height: 360, jpeg: &JPEG });
        assert_eq!(parse(&encode(VideoSource::Camera, 1, 4, 3, &JPEG)).unwrap().source, VideoSource::Camera);
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse(&[]).is_none());
        let mut other = encode(VideoSource::Camera, 1, 4, 3, &JPEG);
        other[0] = 9;
        assert!(parse(&other).is_none(), "another version");
        let mut source = encode(VideoSource::Camera, 1, 4, 3, &JPEG);
        source[1] = 7;
        assert!(parse(&source).is_none(), "an unknown source");
        assert!(parse(&encode(VideoSource::Camera, 1, 0, 3, &JPEG)).is_none(), "no width");
        assert!(parse(&encode(VideoSource::Camera, 1, 4000, 3, &JPEG)).is_none(), "too wide");
        assert!(parse(&encode(VideoSource::Camera, 1, 4, 3, b"\x89PNG..")).is_none(), "not a JPEG");
        let big = encode(VideoSource::Camera, 1, 4, 3, &vec![0xff; MAX_FRAME]);
        assert!(parse(&big).is_none(), "too big");
    }
}
