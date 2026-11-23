//! Finding the other members of a room by its name.
//!
//! On the internet this goes through the public BitTorrent DHT. The room
//! secret gives an ed25519 key; under that key there are a few slots, each
//! holding a short encrypted list of recently online members and their
//! addresses. Anyone who knows the room name and password can read and write
//! those slots; DHT nodes storing them see only noise. Finding one member is
//! enough — the rest come from them.
//!
//! Tests use [`LocalNet`]: an in-process list, no network needed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::{EndpointAddr, TransportAddr};
use mainline::{MutableItem, SigningKey};
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use crate::room::RoomKeys;
use crate::util::hex;

const SLOTS: u8 = 8;
/// BEP44 limit for a stored value.
const MAX_VALUE: usize = 1000;
const ENTRIES_PER_SLOT: usize = 3;
/// Older entries are dropped when a slot is rewritten.
const ENTRY_TTL_SECS: u64 = 30 * 60;
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(20);
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Entry {
    addr: EndpointAddr,
    /// Unix seconds when it was written.
    ts: u64,
}

fn now_secs() -> u64 {
    tovuer_proto::now_ms() / 1000
}

/// Keep what helps others reach us: relay and a few public-ish addresses.
pub fn slim_addr(addr: &EndpointAddr) -> EndpointAddr {
    let mut ips = 0;
    let addrs = addr.addrs.iter().filter(|a| match a {
        TransportAddr::Relay(_) => true,
        TransportAddr::Ip(ip) => {
            let usable = match ip.ip() {
                std::net::IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified(),
                std::net::IpAddr::V6(v6) => !v6.is_loopback() && !v6.is_unspecified() && (v6.segments()[0] & 0xffc0) != 0xfe80,
            };
            ips += usable as usize;
            usable && ips <= 3
        }
        _ => false,
    });
    EndpointAddr::from_parts(addr.id, addrs.cloned())
}

/// In-process "DHT" for tests and local experiments.
#[derive(Clone, Default, Debug)]
pub struct LocalNet {
    rooms: Arc<Mutex<HashMap<String, Vec<EndpointAddr>>>>,
}

impl LocalNet {
    pub fn new() -> Self {
        Self::default()
    }

    fn publish(&self, room: &RoomKeys, addr: EndpointAddr) {
        let mut rooms = self.rooms.lock().unwrap_or_else(|e| e.into_inner());
        let list = rooms.entry(room.id_hex()).or_default();
        list.retain(|a| a.id != addr.id);
        list.push(addr);
    }

    fn lookup(&self, room: &RoomKeys) -> Vec<EndpointAddr> {
        self.rooms.lock().unwrap_or_else(|e| e.into_inner()).get(&room.id_hex()).cloned().unwrap_or_default()
    }

    /// Forget a member (it went offline) — so tests can check reconnection.
    pub fn forget(&self, room: &RoomKeys, id: &iroh::EndpointId) {
        if let Some(list) = self.rooms.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&room.id_hex()) {
            list.retain(|a| a.id != *id);
        }
    }
}

pub enum Discovery {
    Dht(RoomDht),
    Local(LocalNet),
    /// No way to find anyone (the DHT could not start): only remembered peers.
    None,
}

impl Discovery {
    pub async fn lookup(&self, room: &RoomKeys) -> Vec<EndpointAddr> {
        match self {
            Discovery::Dht(dht) => dht.lookup().await,
            Discovery::Local(net) => net.lookup(room),
            Discovery::None => Vec::new(),
        }
    }

    pub async fn publish(&self, room: &RoomKeys, addr: EndpointAddr) {
        match self {
            Discovery::Dht(dht) => dht.publish(addr).await,
            Discovery::Local(net) => net.publish(room, addr),
            Discovery::None => {}
        }
    }
}

pub struct RoomDht {
    dht: mainline::async_dht::AsyncDht,
    keys: Arc<RoomKeys>,
    public: [u8; 32],
}

impl RoomDht {
    /// Start a DHT client (its own UDP socket and thread).
    pub fn start(keys: Arc<RoomKeys>) -> std::io::Result<Self> {
        let dht = mainline::Dht::client()?.as_async();
        let public = SigningKey::from_bytes(&keys.dht_seed()).verifying_key().to_bytes();
        Ok(Self { dht, keys, public })
    }

    fn salt(slot: u8) -> Vec<u8> {
        format!("tovuer-{slot}").into_bytes()
    }

    fn decode(&self, item: &MutableItem) -> Vec<Entry> {
        self.keys
            .open(item.value())
            .and_then(|plain| serde_json::from_slice::<Vec<Entry>>(&plain).ok())
            .unwrap_or_default()
    }

    async fn read_slot(dht: mainline::async_dht::AsyncDht, public: [u8; 32], slot: u8) -> Option<MutableItem> {
        let salt = Self::salt(slot);
        tokio::time::timeout(LOOKUP_TIMEOUT, dht.get_mutable_most_recent(&public, Some(&salt))).await.ok().flatten()
    }

    /// Members seen in the last couple of hours, freshest first.
    pub async fn lookup(&self) -> Vec<EndpointAddr> {
        let mut reads = JoinSet::new();
        for slot in 0..SLOTS {
            reads.spawn(Self::read_slot(self.dht.clone(), self.public, slot));
        }
        let mut found: Vec<Entry> = Vec::new();
        while let Some(done) = reads.join_next().await {
            if let Ok(Some(item)) = done {
                found.extend(self.decode(&item));
            }
        }
        let fresh_after = now_secs().saturating_sub(2 * 3600);
        found.retain(|e| e.ts >= fresh_after);
        found.sort_by_key(|e| std::cmp::Reverse(e.ts));
        let mut seen = std::collections::HashSet::new();
        found.into_iter().filter(|e| seen.insert(e.addr.id)).map(|e| e.addr).collect()
    }

    /// Write ourselves into our slot, keeping the other fresh entries there.
    pub async fn publish(&self, addr: EndpointAddr) {
        let slot = addr.id.as_bytes()[0] % SLOTS;
        let salt = Self::salt(slot);
        let current = Self::read_slot(self.dht.clone(), self.public, slot).await;
        let cas = current.as_ref().map(|item| item.seq());
        let mut entries = current.as_ref().map(|item| self.decode(item)).unwrap_or_default();
        let cutoff = now_secs().saturating_sub(ENTRY_TTL_SECS);
        entries.retain(|e| e.addr.id != addr.id && e.ts >= cutoff);
        entries.insert(0, Entry { addr: slim_addr(&addr), ts: now_secs() });
        entries.truncate(ENTRIES_PER_SLOT);
        let sealed = loop {
            let sealed = self.keys.seal(&serde_json::to_vec(&entries).unwrap_or_default());
            if sealed.len() <= MAX_VALUE || entries.len() <= 1 {
                break sealed;
            }
            entries.pop();
        };
        if sealed.len() > MAX_VALUE {
            return;
        }
        let seq = (now_secs() as i64).max(cas.unwrap_or(0) + 1);
        let item = MutableItem::new(SigningKey::from_bytes(&self.keys.dht_seed()), &sealed, seq, Some(&salt));
        // a concurrent write (CAS conflict) is fine: we try again next round
        let _ = tokio::time::timeout(PUBLISH_TIMEOUT, self.dht.put_mutable(item, cas)).await;
    }

    /// Short id for logs.
    pub fn name(&self) -> String {
        hex(&self.public[..4])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_net_finds_members_of_the_same_room_only() {
        let net = LocalNet::new();
        let (home, other) = (RoomKeys::derive("home", ""), RoomKeys::derive("home", "pw"));
        let a = EndpointAddr::new(iroh::SecretKey::from_bytes(&[1; 32]).public());
        net.publish(&home, a.clone());
        net.publish(&home, a.clone());
        assert_eq!(net.lookup(&home), vec![a.clone()]);
        assert!(net.lookup(&other).is_empty());
        net.forget(&home, &a.id);
        assert!(net.lookup(&home).is_empty());
    }

    #[test]
    fn slim_addr_drops_local_noise() {
        let id = iroh::SecretKey::from_bytes(&[1; 32]).public();
        let addr = EndpointAddr::new(id)
            .with_ip_addr("127.0.0.1:5000".parse().unwrap())
            .with_ip_addr("[fe80::1]:5000".parse().unwrap())
            .with_ip_addr("203.0.113.7:5000".parse().unwrap())
            .with_relay_url("https://relay.example/".parse().unwrap());
        let slim = slim_addr(&addr);
        assert_eq!(slim.ip_addrs().count(), 1);
        assert_eq!(slim.relay_urls().count(), 1);
    }

    #[test]
    fn a_full_slot_fits_the_dht_limit() {
        let keys = RoomKeys::derive("home", "");
        let entries: Vec<Entry> = (1..=ENTRIES_PER_SLOT as u8)
            .map(|n| Entry {
                addr: EndpointAddr::new(iroh::SecretKey::from_bytes(&[n; 32]).public())
                    .with_relay_url("https://euc1-1.relay.n0.iroh-canary.iroh.link./".parse().unwrap())
                    .with_ip_addr("203.0.113.77:51234".parse().unwrap())
                    .with_ip_addr("[2001:db8:1234:5678:9abc:def0:1234:5678]:51234".parse().unwrap()),
                ts: now_secs(),
            })
            .collect();
        let sealed = keys.seal(&serde_json::to_vec(&entries).unwrap());
        assert!(sealed.len() <= MAX_VALUE, "{} bytes", sealed.len());
    }
}
