//! Room identity. Everything is derived from the room name and password:
//! the public room id peers compare, the key that proves membership in the
//! handshake, the key that signs DHT records and the key that encrypts them.
//! Knowing the name and password is what makes you a member.

use crate::util::{hex, random};

pub const MAX_ROOM_CHARS: usize = 48;
/// Key stretching: makes guessing a password from a DHT record slow.
const STRETCH_ROUNDS: u32 = 100_000;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// Normalize a room name the way it was typed: case-insensitive, spaces
/// become dashes, only letters, digits, `-`, `_` and `.` survive.
pub fn normalize_room(raw: &str) -> Option<String> {
    let mut out = String::new();
    for ch in raw.trim().trim_start_matches('#').chars() {
        if ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.extend(ch.to_lowercase());
        } else if ch.is_whitespace() && !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(MAX_ROOM_CHARS).collect();
    (!out.is_empty()).then_some(out)
}

#[derive(Clone)]
pub struct RoomKeys {
    /// Normalized room name.
    pub name: String,
    /// Public room id: both sides of a connection must agree on it.
    pub id: [u8; 32],
    auth: [u8; 32],
    dht_seed: [u8; 32],
    seal_key: [u8; 32],
    mac_key: [u8; 32],
}

impl std::fmt::Debug for RoomKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomKeys").field("name", &self.name).field("id", &hex(&self.id[..8])).finish_non_exhaustive()
    }
}

impl RoomKeys {
    /// Derive the keys. Takes a few milliseconds on purpose (key stretching).
    pub fn derive(name: &str, password: &str) -> Self {
        let mut material = Vec::with_capacity(name.len() + password.len() + 1);
        material.extend_from_slice(name.as_bytes());
        material.push(0);
        material.extend_from_slice(password.as_bytes());
        let mut secret = blake3::derive_key("tovuer room secret v1", &material);
        for _ in 0..STRETCH_ROUNDS {
            secret = *blake3::keyed_hash(&secret, &material).as_bytes();
        }
        let sub = |context: &str| blake3::derive_key(context, &secret);
        Self {
            name: name.to_string(),
            id: sub("tovuer room id v1"),
            auth: sub("tovuer room auth v1"),
            dht_seed: sub("tovuer room dht v1"),
            seal_key: sub("tovuer room seal v1"),
            mac_key: sub("tovuer room mac v1"),
        }
    }

    pub fn id_hex(&self) -> String {
        hex(&self.id)
    }

    /// Folder name for this room's data on disk.
    pub fn dir_name(&self) -> String {
        hex(&self.id[..8])
    }

    /// Proof that `from` knows the room secret, bound to both endpoint ids so
    /// it cannot be replayed to someone else.
    pub fn proof(&self, from: &[u8; 32], to: &[u8; 32]) -> [u8; 32] {
        let mut h = blake3::Hasher::new_keyed(&self.auth);
        h.update(b"tovuer hello");
        h.update(from);
        h.update(to);
        *h.finalize().as_bytes()
    }

    /// Seed of the ed25519 key that signs this room's DHT records.
    pub fn dht_seed(&self) -> [u8; 32] {
        self.dht_seed
    }

    fn keystream(&self, nonce: &[u8], len: usize) -> Vec<u8> {
        let mut h = blake3::Hasher::new_keyed(&self.seal_key);
        h.update(nonce);
        let mut out = vec![0u8; len];
        h.finalize_xof().fill(&mut out);
        out
    }

    fn tag(&self, nonce_and_body: &[u8]) -> [u8; TAG_LEN] {
        let full = blake3::keyed_hash(&self.mac_key, nonce_and_body);
        let mut tag = [0u8; TAG_LEN];
        tag.copy_from_slice(&full.as_bytes()[..TAG_LEN]);
        tag
    }

    /// Encrypt and authenticate a DHT record: DHT nodes storing it must not
    /// learn who is in the room or their addresses.
    pub fn seal(&self, plain: &[u8]) -> Vec<u8> {
        let nonce = random::<NONCE_LEN>();
        let mut out = nonce.to_vec();
        out.extend(plain.iter().zip(self.keystream(&nonce, plain.len())).map(|(a, b)| a ^ b));
        let tag = self.tag(&out);
        out.extend_from_slice(&tag);
        out
    }

    pub fn open(&self, sealed: &[u8]) -> Option<Vec<u8>> {
        if sealed.len() < NONCE_LEN + TAG_LEN {
            return None;
        }
        let (body, tag) = sealed.split_at(sealed.len() - TAG_LEN);
        if self.tag(body) != tag {
            return None;
        }
        let (nonce, cipher) = body.split_at(NONCE_LEN);
        Some(cipher.iter().zip(self.keystream(nonce, cipher.len())).map(|(a, b)| a ^ b).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_names_are_normalized() {
        assert_eq!(normalize_room("  Our Place ").as_deref(), Some("our-place"));
        assert_eq!(normalize_room("#Friends_2.0").as_deref(), Some("friends_2.0"));
        assert_eq!(normalize_room("a/b\\c"), Some("abc".into()));
        assert_eq!(normalize_room("   "), None);
        assert_eq!(normalize_room(&"x".repeat(100)).unwrap().len(), MAX_ROOM_CHARS);
    }

    #[test]
    fn keys_depend_on_name_and_password() {
        let a = RoomKeys::derive("home", "");
        assert_eq!(a.id, RoomKeys::derive("home", "").id);
        assert_ne!(a.id, RoomKeys::derive("home", "secret").id);
        assert_ne!(a.id, RoomKeys::derive("home2", "").id);
        // the separator keeps "ab"+"c" apart from "a"+"bc"
        assert_ne!(RoomKeys::derive("ab", "c").id, RoomKeys::derive("a", "bc").id);
    }

    #[test]
    fn proofs_are_directional() {
        let k = RoomKeys::derive("home", "");
        let (x, y) = ([1u8; 32], [2u8; 32]);
        assert_eq!(k.proof(&x, &y), k.proof(&x, &y));
        assert_ne!(k.proof(&x, &y), k.proof(&y, &x));
        assert_ne!(k.proof(&x, &y), RoomKeys::derive("home", "pw").proof(&x, &y));
    }

    #[test]
    fn seal_roundtrip_and_tamper() {
        let k = RoomKeys::derive("home", "");
        let sealed = k.seal(b"hello peers");
        assert_eq!(k.open(&sealed).as_deref(), Some(&b"hello peers"[..]));
        assert_ne!(&sealed[NONCE_LEN..NONCE_LEN + 11], b"hello peers");
        let mut broken = sealed.clone();
        broken[NONCE_LEN] ^= 1;
        assert_eq!(k.open(&broken), None);
        assert_eq!(RoomKeys::derive("other", "").open(&sealed), None);
        assert_eq!(k.open(&[1, 2, 3]), None);
    }
}
