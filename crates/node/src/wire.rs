//! What peers say to each other.
//!
//! One QUIC connection per pair of peers. Every bidirectional stream starts
//! with a one-byte tag: `C` — the control stream (handshake, then frames for
//! the whole life of the connection), `F` — a file request. Frames are a 4-byte big-endian length and JSON. Voice goes as
//! unreliable QUIC datagrams in the [`tovuer_proto::voice`] format; video
//! as one unidirectional stream per frame: the tag `V`, then a
//! [`tovuer_proto::video`] frame.

use std::collections::BTreeMap;

use iroh::EndpointAddr;
use iroh::endpoint::{RecvStream, SendStream};
use serde::{Deserialize, Serialize};
use tovuer_proto::VideoSource;

use crate::ops::Op;

pub const ALPN: &[u8] = b"tovuer/4";
pub const TAG_CONTROL: u8 = b'C';
pub const TAG_FILE: u8 = b'F';
pub const TAG_VIDEO: u8 = b'V';
/// Biggest frame we accept (a batch of operations).
pub const MAX_FRAME: usize = 4 << 20;
/// Operations per `Ops` frame when catching someone up.
pub const OPS_BATCH: usize = 256;

/// Presence: who the peer is right now. Not stored anywhere.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct PeerState {
    pub nick: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    pub muted: bool,
    pub deafened: bool,
    /// Camera or screen on, in the voice channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoSource>,
    /// Looking at the call: wants the video of the channel.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub watching: bool,
    /// Watching this member's screen from a text channel (their nick).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<String>,
    pub version: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Frame {
    /// First frame on the control stream, both ways.
    Hello {
        proto: u32,
        /// Room id, hex.
        room: String,
        /// Proof of knowing the room secret, hex.
        proof: String,
        state: PeerState,
        /// Where the sender can be reached — for others to dial it.
        addr: EndpointAddr,
    },
    /// The other side refuses to talk (wrong room, banned, old protocol).
    Reject { reason: String },
    State { state: PeerState },
    /// Last operation number we have from each author: "send me what is newer".
    Heads { heads: BTreeMap<String, u64> },
    Ops { ops: Vec<Op> },
    /// Done sending what the other side asked for in `Heads`.
    Synced,
    /// Other members we know about.
    Peers { addrs: Vec<EndpointAddr> },
    Typing { channel: String },
    Speaking { on: bool },
    FileReq { id: String },
    /// Answer to a file request; the bytes follow the frame.
    Blob { size: u64 },
    Missing,
    #[serde(other)]
    Unknown,
}

pub async fn write_frame(send: &mut SendStream, frame: &Frame) -> std::io::Result<()> {
    let body = serde_json::to_vec(frame).map_err(std::io::Error::other)?;
    let mut buf = Vec::with_capacity(body.len() + 4);
    buf.extend_from_slice(&(body.len() as u32).to_be_bytes());
    buf.extend_from_slice(&body);
    send.write_all(&buf).await.map_err(std::io::Error::other)
}

/// Read one frame. `Ok(None)` — the stream ended cleanly.
pub async fn read_frame(recv: &mut RecvStream) -> std::io::Result<Option<Frame>> {
    let mut len = [0u8; 4];
    match recv.read_exact(&mut len).await {
        Ok(()) => {}
        Err(iroh::endpoint::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(e) => return Err(std::io::Error::other(e)),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::other(format!("frame too big: {len}")));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.map_err(std::io::Error::other)?;
    // a frame we cannot parse is skipped, not fatal
    Ok(Some(serde_json::from_slice(&body).unwrap_or(Frame::Unknown)))
}
