//! The binary voice frame, sent between peers as a QUIC datagram.
//!
//! ```text
//! [0]     version (3)
//! [1]     flags (bit 0 — last frame of a phrase)
//! [2..6]  sender id, u32 BE (0 on the wire; the receiver fills in its own id for the sender)
//! [6..8]  sequence number, u16 BE
//! [8..]   Opus packet (48 kHz, mono, 20 ms)
//! ```

pub const VERSION: u8 = 3;
pub const HEADER_LEN: usize = 8;
/// Largest Opus packet per RFC 6716.
pub const MAX_PAYLOAD: usize = 1275;

/// Last frame of a phrase: the sender goes quiet now, so the listener's buffer
/// may run empty — that is not a lost connection.
pub const FLAG_END: u8 = 0x01;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    pub flags: u8,
    pub sender: u32,
    pub seq: u16,
    pub payload: &'a [u8],
}

impl Frame<'_> {
    pub fn is_end(&self) -> bool {
        self.flags & FLAG_END != 0
    }
}

/// Build a frame. `sender` is 0 on the wire.
pub fn encode(flags: u8, sender: u32, seq: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.push(VERSION);
    out.push(flags);
    out.extend_from_slice(&sender.to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Parse a frame; `None` — another version, an empty or an oversized packet.
pub fn parse(buf: &[u8]) -> Option<Frame<'_>> {
    if buf.len() <= HEADER_LEN || buf.len() > HEADER_LEN + MAX_PAYLOAD || buf[0] != VERSION {
        return None;
    }
    Some(Frame {
        flags: buf[1],
        sender: u32::from_be_bytes([buf[2], buf[3], buf[4], buf[5]]),
        seq: u16::from_be_bytes([buf[6], buf[7]]),
        payload: &buf[HEADER_LEN..],
    })
}

/// Check a frame and write the real sender into it. `false` — the frame is
/// no good and must be dropped.
pub fn stamp_sender(buf: &mut [u8], sender: u32) -> bool {
    if parse(buf).is_none() {
        return false;
    }
    buf[2..6].copy_from_slice(&sender.to_be_bytes());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let buf = encode(FLAG_END, 0, 65535, &[1, 2, 3]);
        let f = parse(&buf).unwrap();
        assert_eq!(f, Frame { flags: FLAG_END, sender: 0, seq: 65535, payload: &[1, 2, 3] });
        assert!(f.is_end());
    }

    #[test]
    fn stamping_sets_the_sender() {
        let mut buf = encode(0, 0xdead_beef, 7, &[9; 40]);
        assert!(stamp_sender(&mut buf, 42));
        let f = parse(&buf).unwrap();
        assert_eq!((f.sender, f.seq, f.payload.len()), (42, 7, 40));
        assert!(!f.is_end());
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[VERSION, 0, 0, 0, 0, 0, 0, 0]).is_none(), "empty payload");
        assert!(parse(&[2, 0, 0, 0, 0, 0, 0, 0, 1]).is_none(), "old version");
        let big = encode(0, 0, 0, &vec![0u8; MAX_PAYLOAD + 1]);
        assert!(parse(&big).is_none());
        let mut junk = vec![0x02, 4, b'd', b'i', b'a', b's', 0, 1, 0xff];
        assert!(!stamp_sender(&mut junk, 1), "a frame of another version does not pass");
    }
}
