//! tovuer peer-to-peer core. There is no server: every running client is a
//! [`Node`] that joins a room by its name and password, finds the other
//! members through the public DHT, connects to each of them directly
//! (iroh: QUIC with hole punching, relays as a fallback) and keeps a full copy
//! of the room. Voice goes straight from the speaker to every listener.

pub mod discovery;
pub mod log;
pub mod node;
pub mod ops;
pub mod room;
pub mod state;
pub mod util;
pub mod wire;

pub use discovery::LocalNet;
pub use iroh::SecretKey;
pub use node::{Config, Event, MAX_FILE, Network, Node, VoiceSink};
pub use room::normalize_room;

/// Load this install's identity from `path`, creating it on first run.
pub fn load_identity(path: &std::path::Path) -> std::io::Result<SecretKey> {
    if let Ok(text) = std::fs::read_to_string(path)
        && let Some(bytes) = util::unhex::<32>(text.trim())
    {
        return Ok(SecretKey::from_bytes(&bytes));
    }
    let secret = SecretKey::from_bytes(&util::random::<32>());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, util::hex(&secret.to_bytes()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(secret)
}
