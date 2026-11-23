//! Two peers on the real internet find each other by room name and talk:
//! `cargo run -p tovuer-node --example smoke` (needs network access).

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tovuer_node::util::{hex, random};
use tovuer_node::{Config, Event, Network, Node, SecretKey};
use tovuer_proto::{ClientMsg, ServerMsg};

async fn peer(room: &str, nick: &str, dir: &std::path::Path) -> anyhow::Result<(Node, mpsc::UnboundedReceiver<Event>)> {
    let (tx, rx) = mpsc::unbounded_channel();
    let cfg = Config {
        room: room.into(),
        password: "smoke".into(),
        nick: nick.into(),
        version: "smoke".into(),
        secret: SecretKey::from_bytes(&random::<32>()),
        data_dir: dir.join(nick).join("rooms"),
        files_dir: dir.join(nick).join("files"),
        release_dir: None,
        network: Network::Internet,
    };
    Ok((Node::start(cfg, tx, Arc::new(|_, _, _, _| {})).await?, rx))
}

async fn wait(rx: &mut mpsc::UnboundedReceiver<Event>, what: &str, limit: Duration, want: impl Fn(&ServerMsg) -> bool) -> anyhow::Result<ServerMsg> {
    let started = Instant::now();
    let found = tokio::time::timeout(limit, async {
        while let Some(event) = rx.recv().await {
            match event {
                Event::Msg(msg) if want(&msg) => return Some(*msg),
                Event::Rejected { msg, .. } => panic!("rejected: {msg}"),
                _ => {}
            }
        }
        None
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out waiting for {what}"))?
    .ok_or_else(|| anyhow::anyhow!("node stopped"))?;
    println!("  {what}: {:.1} s", started.elapsed().as_secs_f32());
    Ok(found)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let room = format!("smoke-{}", hex(&random::<6>()));
    let dir = std::env::temp_dir().join(format!("tovuer-smoke-{}", hex(&random::<4>())));
    println!("room {room}");

    let (a, mut a_rx) = peer(&room, "alice", &dir).await?;
    let welcome = |m: &ServerMsg| matches!(m, ServerMsg::Welcome { .. });
    wait(&mut a_rx, "alice opened the room", Duration::from_secs(60), welcome).await?;
    // give alice's DHT record a moment to land
    tokio::time::sleep(Duration::from_secs(5)).await;

    let (b, mut b_rx) = peer(&room, "bob", &dir).await?;
    let ServerMsg::Welcome { users, .. } = wait(&mut b_rx, "bob joined", Duration::from_secs(60), welcome).await? else { unreachable!() };
    if !users.iter().any(|u| u.name == "alice") {
        wait(&mut b_rx, "bob found alice", Duration::from_secs(180), |m| matches!(m, ServerMsg::UserJoin { user } if user.name == "alice")).await?;
    } else {
        println!("  bob found alice while joining");
    }

    a.send(ClientMsg::Send { channel: "general".into(), text: "hello over the internet".into(), reply_to: None });
    wait(&mut b_rx, "message alice → bob", Duration::from_secs(30), |m| matches!(m, ServerMsg::Msg { msg } if msg.text == "hello over the internet")).await?;
    b.send(ClientMsg::Send { channel: "general".into(), text: "hi back".into(), reply_to: None });
    wait(&mut a_rx, "message bob → alice", Duration::from_secs(30), |m| matches!(m, ServerMsg::Msg { msg } if msg.text == "hi back")).await?;

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        if let Ok(Some(Event::Latency(Some(rtt)))) = tokio::time::timeout(Duration::from_secs(4), b_rx.recv()).await {
            println!("  round trip bob ↔ alice: {} ms", rtt.as_millis());
            break;
        }
    }
    a.shutdown().await;
    b.shutdown().await;
    let _ = std::fs::remove_dir_all(dir);
    println!("ok");
    Ok(())
}
