//! Operations: the room's replicated log. Every change (a message, an edit,
//! a new channel, a ban) is an operation signed by its author. Each author
//! numbers their own operations 1, 2, 3…, so peers can tell exactly what the
//! other side is missing. Operations pass through other peers untouched — the
//! signature proves who wrote them.

use iroh::{PublicKey, SecretKey, Signature};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tovuer_proto::{ChannelKind, FileMeta, ReplyRef};

use crate::room::RoomKeys;
use crate::util::{canonical_json, hex, unhex};

pub type Key = [u8; 32];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Op {
    /// Author's public key, hex.
    pub author: String,
    pub seq: u64,
    /// Hybrid clock, milliseconds: never behind anything the author has seen.
    pub ts: u64,
    /// Author's nick when the operation was made.
    pub nick: String,
    /// Kept as raw JSON so operations from newer versions pass through intact.
    pub body: Value,
    /// Signature, hex.
    pub sig: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Body {
    /// The room was created; its author owns the room.
    Genesis { name: String },
    Msg {
        channel: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply: Option<ReplyRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<FileMeta>,
    },
    Edit { id: u64, text: String },
    Delete { id: u64 },
    React { id: u64, emoji: String, on: bool },
    ChannelCreate { name: String, kind: ChannelKind },
    ChannelTopic { name: String, topic: String },
    ChannelDelete { name: String },
    /// Owner removes a member: peers stop talking to that key.
    Ban { key: String, name: String },
    /// Something from a newer version.
    #[serde(other)]
    Unknown,
}

fn signable(room: &RoomKeys, author: &str, seq: u64, ts: u64, nick: &str, body: &Value) -> Vec<u8> {
    canonical_json(&json!([room.id_hex(), author, seq, ts, nick, body])).into_bytes()
}

impl Op {
    pub fn new(secret: &SecretKey, room: &RoomKeys, seq: u64, ts: u64, nick: &str, body: &Body) -> Op {
        let author = hex(secret.public().as_bytes());
        let body = serde_json::to_value(body).unwrap_or(Value::Null);
        let sig = secret.sign(&signable(room, &author, seq, ts, nick, &body));
        Op { author, seq, ts, nick: nick.to_string(), body, sig: hex(&sig.to_bytes()) }
    }

    pub fn author_key(&self) -> Option<Key> {
        unhex::<32>(&self.author)
    }

    /// Signature is valid for this room and this author.
    pub fn verify(&self, room: &RoomKeys) -> bool {
        let (Some(key), Some(sig)) = (self.author_key(), unhex::<64>(&self.sig)) else { return false };
        let Ok(public) = PublicKey::from_bytes(&key) else { return false };
        let sig = Signature::from_bytes(&sig);
        self.seq > 0 && public.verify(&signable(room, &self.author, self.seq, self.ts, &self.nick, &self.body), &sig).is_ok()
    }

    pub fn body(&self) -> Body {
        serde_json::from_value(self.body.clone()).unwrap_or(Body::Unknown)
    }

    /// Message id of this operation (see [`msg_id`]).
    pub fn msg_id(&self) -> u64 {
        msg_id(&self.author_key().unwrap_or_default(), self.seq, self.ts)
    }
}

/// Message ids grow with time — clients keep messages sorted by id — and the
/// low bits come from the author and number, so two peers never collide.
pub fn msg_id(author: &Key, seq: u64, ts: u64) -> u64 {
    let mut h = blake3::Hasher::new();
    h.update(author);
    h.update(&seq.to_be_bytes());
    let h = h.finalize();
    let low = u16::from_be_bytes([h.as_bytes()[0], h.as_bytes()[1]]) as u64;
    (ts.min((1 << 48) - 1) << 16) | low
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_are_signed_for_one_room() {
        let room = RoomKeys::derive("home", "");
        let secret = SecretKey::from_bytes(&[7u8; 32]);
        let body = Body::Msg { channel: "general".into(), text: "hi".into(), reply: None, file: None };
        let op = Op::new(&secret, &room, 1, 1_700_000_000_000, "dana", &body);
        assert!(op.verify(&room));
        assert_eq!(op.body(), body);
        assert_eq!(op.author_key(), Some(*secret.public().as_bytes()));

        // survives a trip through JSON, even with keys reordered
        let wire = serde_json::to_string(&op).unwrap();
        let back: Op = serde_json::from_str(&wire).unwrap();
        assert!(back.verify(&room));

        assert!(!op.verify(&RoomKeys::derive("other", "")), "another room");
        let mut forged = op.clone();
        forged.nick = "admin".into();
        assert!(!forged.verify(&room), "nick is signed");
        let mut forged = op.clone();
        forged.body = serde_json::json!({"t": "msg", "channel": "general", "text": "hacked"});
        assert!(!forged.verify(&room), "body is signed");
    }

    #[test]
    fn unknown_bodies_pass_through() {
        let room = RoomKeys::derive("home", "");
        let secret = SecretKey::from_bytes(&[1u8; 32]);
        let mut op = Op::new(&secret, &room, 1, 5, "x", &Body::Delete { id: 1 });
        op.body = serde_json::json!({"t": "from_the_future", "z": 1, "a": [2]});
        op = Op { sig: Op::new(&secret, &room, 1, 5, "x", &Body::Unknown).sig, ..op };
        assert_eq!(op.body(), Body::Unknown);
    }

    #[test]
    fn message_ids_follow_time() {
        let a = msg_id(&[1; 32], 1, 1000);
        let b = msg_id(&[2; 32], 1, 1000);
        let c = msg_id(&[1; 32], 2, 1001);
        assert_ne!(a, b);
        assert!(a < c && b < c);
        assert_eq!(a >> 16, 1000);
    }
}
