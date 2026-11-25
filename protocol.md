# The tovuer protocol (v4)

There is no server. Every running client is a peer that keeps a full copy of
the room, connects straight to every other member it can find and sends its
own voice and video directly to the people in the same voice channel. The code lives in
`crates/node`; the shared types are in `crates/proto`.

## Rooms

A room is a **name** and an optional **password**. Everyone who types the same
pair ends up in the same room; anyone who does not know them cannot find it,
join it or read anything about it.

The name is normalized first (lowercase, spaces become `-`, only letters,
digits, `-`, `_`, `.`). Then:

```
secret   = blake3 key stretching of (name ‖ 0x00 ‖ password), 100 000 rounds
room id  = derive_key("tovuer room id v1",   secret)   — compared in the handshake
auth key = derive_key("tovuer room auth v1", secret)   — proves membership
dht seed = derive_key("tovuer room dht v1",  secret)   — ed25519 key of the DHT records
seal key = derive_key("tovuer room seal v1", secret)   — encrypts the DHT records
```

A wrong password is simply a different, empty room.

## Finding each other

Peers find the room through the public BitTorrent DHT (BEP 44 mutable items,
the [`mainline`](https://crates.io/crates/mainline) crate). Under the room's
DHT key there are 8 slots (salt `tovuer-0` … `tovuer-7`); a peer writes itself
into slot `id[0] % 8`, keeping up to two other fresh entries already there:

```json
[{"addr": {"id": "<endpoint id>", "addrs": [{"Relay": "https://…"}, {"Ip": "203.0.113.7:41641"}]}, "ts": 1763820000}]
```

The JSON is encrypted (blake3 keystream, 12-byte nonce, 16-byte tag) so the
DHT nodes storing it learn nothing about the members. Records are rewritten
every 5 minutes; entries older than 30 minutes are dropped on rewrite and
those older than 2 hours are ignored on lookup.

Finding one member is enough: on connecting, peers send each other the
addresses of everyone else they are connected to. Each peer also remembers
everyone it has talked to (`peers.json`) and tries them first next time.

## Connections

Peers connect with [iroh](https://iroh.computer): QUIC between endpoints
identified by their ed25519 public key, with NAT hole punching and the
public n0 relays as a fallback (relays only ever see encrypted traffic). The
ALPN is `tovuer/4`. Each install has one key, kept in `identity.key` in the
config folder.

Every bidirectional stream starts with a one-byte tag:

| Tag | Stream |
|---|---|
| `C` | control: the handshake, then frames for the life of the connection |
| `F` | a file request |

Frames are a 4-byte big-endian length followed by JSON tagged with `t`.
Frames a peer does not understand are skipped. Unidirectional streams carry
video, one frame each (see [Video](#video)).

### Handshake

The dialer opens the control stream and sends `hello`; the other side checks
it and answers with its own `hello`, or with `reject` and closes.

```json
{"t":"hello","proto":4,"room":"<room id hex>","proof":"<hex>",
 "state":{"nick":"dana","version":"20261122-1800","voice":"Lounge","muted":false,"deafened":false},
 "addr":{"id":"…","addrs":[…]}}
```

`proof = keyed_blake3(auth key, "tovuer hello" ‖ sender id ‖ receiver id)` —
it proves knowledge of the room secret and cannot be replayed to anyone else
(the endpoint ids are authenticated by QUIC itself). If both peers dial each
other at the same moment, the connection dialed by the smaller id is kept; a
connection older than a few seconds is treated as stale (the peer restarted)
and replaced by the new one.

### Control frames

| `t` | Fields | Meaning |
|---|---|---|
| `state` | `state` | presence: nick, voice channel, mute, camera or screen on (`video`), watching the call (`watching`), whose screen it watches (`stream`), version |
| `heads` | `heads` | the last operation number we have from each author |
| `ops` | `ops` | operations the other side lacks (in batches of up to 256) |
| `synced` | — | done answering a `heads` |
| `peers` | `addrs` | other members we are connected to |
| `typing` | `channel` | "typing…" |
| `speaking` | `on` | started / stopped talking |

Right after the handshake both sides send `state`, `heads` and `peers`. Each
side answers `heads` with the operations the other is missing, then `synced`.
`heads` is exchanged again every minute, and immediately when an operation
arrives out of order.

## The operation log

Everything that changes the room is an **operation**, signed by its author:

```json
{"author":"<public key hex>","seq":42,"ts":1763820000123,"nick":"dana",
 "body":{"t":"msg","channel":"general","text":"hi"},"sig":"<ed25519 hex>"}
```

- `seq` counts each author's operations from 1 without gaps, so a peer
  accepts them strictly in order and knows exactly what it is missing.
- `ts` is a hybrid clock in milliseconds: never behind anything the author
  has seen.
- The signature covers `[room id, author, seq, ts, nick, body]` serialized as
  JSON with sorted keys. Operations pass through other peers untouched; the
  signature proves who wrote them. Bodies from newer versions are kept and
  passed on even if not understood.

| `body.t` | Fields | Who may | Meaning |
|---|---|---|---|
| `genesis` | `name` | anyone | creates the room; the author of the **earliest** genesis owns it |
| `msg` | `channel`, `text?`, `reply?`, `file?`, `style?` | anyone | a message |
| `edit` | `id`, `text` | the author | last write wins |
| `delete` | `id` | the author or the owner | |
| `react` | `id`, `emoji`, `on` | anyone | per person and emoji, last write wins |
| `channel_create` | `name`, `kind` | anyone | `text` or `voice` |
| `channel_topic` | `name`, `topic` | anyone | last write wins |
| `channel_delete` | `name` | the owner | |
| `ban` | `key`, `name` | the owner | peers stop talking to that key and ignore its newer operations |

`style` is how the author wants the text to look; readers draw what they
understand of it and ignore the rest:

```json
{"color": "#ff8800", "gradient": ["#f72585", "#7209b7", "#4cc9f0"], "flow": true,
 "bold": true, "italic": false, "strike": false, "code": false}
```

`gradient` (2–4 colors) wins over `color`; `flow` makes it run along the
text. Every field is optional, and invalid colors are dropped.

Every peer folds the same operations into the same room. Operations on a
message that has not arrived yet wait for it; a delete that arrives first
wins. When an earlier genesis shows up (someone opened the room while the
lookup had not found the others yet), the room is folded again from scratch
with the right owner.

**Message ids** are `(ts << 16) | first two bytes of blake3(author ‖ seq)`:
they grow with time, so the client keeps messages sorted by id, and two peers
never collide.

The log is stored in `rooms/<room id prefix>/ops.jsonl` in the data folder,
one operation per line. Nothing is ever lost while any member keeps a copy.

## Voice

Voice frames are QUIC datagrams (unreliable — late audio is useless) sent
directly from the speaker to every peer in the same voice channel that has
not switched sound off:

```
[0]     version (3)
[1]     flags: bit 0 — last frame of a phrase
[2..6]  sender id, u32 BE (0 on the wire; the receiver uses its own id for the sender)
[6..8]  sequence number, u16 BE
[8..]   Opus packet: 48 kHz, mono, 20 ms
```

A client sends frames only while talking (the detector is RNNoise). The last
frame of a phrase is flagged, so the listener knows silence follows rather
than loss. Numbers run on without gaps across pauses: the listener sees lost
packets (the decoder conceals them), and a late packet takes its place if
that is not played yet. Each speaker sends their own voice to each listener,
so the load is spread over everyone: about 4 KB/s per listener while talking.

## Video

In a voice channel a peer can show its camera or its screen (one at a time)
and open the call to watch the others. Both are part of `state`:
`"video":"camera"` or `"screen"` — what it shows; `"watching":true` — it
looks at the call. Video goes only to peers in the same voice channel who
are watching, so a call nobody looks at costs nothing.

A shared screen is also a stream for the whole room: any peer, in voice or
not, may set `"stream":"<nick>"` in its `state` and gets that member's
screen — only the screen, never the camera.

Each frame travels on a unidirectional stream of its own: the byte `V`, then

```
[0]      version (1)
[1]      source: 0 — a camera, 1 — a screen
[2..6]   sequence number, u32 BE
[6..8]   width, u16 BE  (up to 1920)
[8..10]  height, u16 BE (up to 1200)
[10..]   a JPEG of that size (a frame is up to 768 KB)
```

A stream per frame means a late frame never holds up a newer one. The
sender keeps at most six frames on their way to each peer; frames that come
while all are travelling wait, and only the newest of them goes — a slow
link gets fewer frames instead of growing lag. A camera is sent at up to
480×360, 30 frames a second (about 300–450 KB/s per watcher). Screen sharing
targets 60 frames a second at up to 1440×900. Bandwidth per watcher depends
on the size of the JPEGs and how often the picture changes. The sender
repeats a still frame every two seconds so new watchers can see it.
The receiver drops frames from anyone it is
not watching (in its call, or as the `stream` it chose — then a screen only),
and a JPEG whose real size differs from the header.

## Files

A shared file is named by its content hash (the first 16 bytes of blake3,
hex) and stored in the `files` folder. The `msg` operation carries
`{"id","name","size","image"}`; the bytes are fetched on demand from the
author first, then from anyone else online — everyone who downloads a file
can hand it on.

```
F  {"t":"file_req","id":"<hex>"}
→  {"t":"blob","size":183420} + the bytes      or      {"t":"missing"}
```

The receiver checks the hash before keeping the file. Files are up to 50 MB.

## Updates

Peers do not hand out builds. `/update` takes the newest release from the
project's GitHub releases (`api.github.com/repos/dszhlt/tovuer/releases/latest`):
the file for its system (`tovuer-linux-amd64`, `tovuer-windows-amd64.exe`)
from the release's own download address, checked against its size and the
SHA-256 GitHub gives for it, and that it is a program for this system,
before it replaces the client. Versions are compared by number and never go
back. Releases are built by GitHub Actions from a `v*` tag.

## Limits

Video is a mesh like voice: whoever shows video sends it to every watcher,
so it suits a few cameras at a time.

Nick — up to 24 characters without spaces; channel name — up to 24 (letters,
digits, `-`, `_`); text — up to 4000 characters; topic — up to 120; up to 20
different reactions per message; up to 50 visible channels. When concurrent
creations exceed the room limit, peers show the 50 channels with the earliest
first creation times, breaking ties by channel name. A remaining channel can
become visible when a slot opens. The owner's deletion is remembered even
when the corresponding creation has not reached a peer yet.
