//! tovuer — the terminal client, and a peer of the room at the same time.
//! Run `tovuer` (asks for a nick and a room) or `tovuer --room our-place --name dana`.
//! `tovuer --install` puts the `tovuer` command in your PATH.

use std::io::{Write, stdout};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange, EnableMouseCapture, Event,
    EventStream, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode};
use crossterm::{clipboard::CopyToClipboard, execute};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;
use tovuer::app::{App, Effect, Launch};
use tovuer::net::{self, NetHandle, NodeSetup};
use tovuer::settings::Paths;
use tovuer::{VERSION, transfer, ui};
use tovuer_proto::voice;
use tovuer_voice::{Engine, Packet};

/// Baked into a build for friends (`./run.sh friend <room>`): the room, its
/// password and "ask only for a nick".
const BAKED_ROOM: Option<&str> = option_env!("TOVUER_ROOM");
const BAKED_PASSWORD: Option<&str> = option_env!("TOVUER_ROOM_PASSWORD");
const BAKED_FRIEND: Option<&str> = option_env!("TOVUER_FRIEND");

#[derive(Parser, Debug)]
#[command(name = "tovuer", version = VERSION, about = "tovuer — voice and text chat right in the terminal, peer to peer")]
struct Args {
    /// Room to join: everyone who types the same name (and password) ends up together
    #[arg(long)]
    room: Option<String>,
    /// Nick — log in right away, skipping the login screen
    #[arg(long)]
    name: Option<String>,
    /// Room password, if it has one
    #[arg(long, env = "TOVUER_PASSWORD", hide_env_values = true)]
    password: Option<String>,
    /// Microphone: part of its name (see --list-devices)
    #[arg(long)]
    mic: Option<String>,
    /// Speakers: part of their name (see --list-devices)
    #[arg(long)]
    spk: Option<String>,
    /// List audio devices and exit
    #[arg(long)]
    list_devices: bool,
    /// Install the tovuer command: from then on just type tovuer in a terminal
    #[arg(long)]
    install: bool,
    /// Settings folder (default — the system one)
    #[arg(long, value_name = "DIR")]
    config_dir: Option<PathBuf>,
}

fn list_devices() -> anyhow::Result<()> {
    let (mics, spks) = tovuer_voice::list_devices().map_err(|e| anyhow::anyhow!("audio: {e}"))?;
    for (title, list) in [("microphones", mics), ("speakers", spks)] {
        println!("{title}:");
        for device in list {
            println!(" {} {}", if device.is_default { "*" } else { " " }, device.name);
        }
    }
    println!("\n* — system default; pick with --mic <part of the name> / --spk <part of the name>");
    Ok(())
}

/// The terminal in full-screen mode; on exit (and on a panic) everything goes back.
struct Tui {
    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
    enhanced_keys: bool,
    #[cfg(unix)]
    saved_stderr: Option<i32>,
}

/// Put the terminal back to normal. Safe to call twice.
fn restore_terminal(enhanced_keys: bool) {
    let mut out = stdout();
    if enhanced_keys {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(out, DisableMouseCapture, DisableBracketedPaste, DisableFocusChange, LeaveAlternateScreen, crossterm::cursor::Show);
    let _ = disable_raw_mode();
}

impl Tui {
    fn enter(mouse: bool, log_dir: &std::path::Path) -> anyhow::Result<Self> {
        // Audio libraries like to print warnings to stderr — garbage on top of
        // the interface. While running, stderr goes to a file.
        #[cfg(unix)]
        let saved_stderr = redirect_stderr(log_dir);
        #[cfg(not(unix))]
        let _ = log_dir;

        enable_raw_mode().context("the terminal will not switch to raw mode — is this a terminal?")?;
        let mut out = stdout();
        execute!(out, EnterAlternateScreen, EnableBracketedPaste, EnableFocusChange)?;
        if mouse {
            execute!(out, EnableMouseCapture)?;
        }
        // the extended keyboard protocol (kitty, ghostty, foot, wezterm…):
        // with it the terminal tells Shift+Enter apart and reports Alt combinations
        let enhanced_keys = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
        if enhanced_keys {
            execute!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
        }
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal(enhanced_keys);
            hook(info);
        }));
        let terminal = Terminal::new(CrosstermBackend::new(out))?;
        Ok(Self {
            terminal,
            enhanced_keys,
            #[cfg(unix)]
            saved_stderr,
        })
    }

    fn apply(&mut self, effect: Effect) {
        let mut out = stdout();
        let _ = match effect {
            Effect::Bell => out.write_all(b"\x07").and_then(|()| out.flush()),
            Effect::Copy(text) => execute!(out, CopyToClipboard::to_clipboard_from(text)),
            Effect::Title(title) => execute!(out, SetTitle(title)),
            Effect::Mouse(true) => execute!(out, EnableMouseCapture),
            Effect::Mouse(false) => execute!(out, DisableMouseCapture),
        };
    }

    fn leave(self) {
        restore_terminal(self.enhanced_keys);
        #[cfg(unix)]
        if let Some(fd) = self.saved_stderr {
            // SAFETY: fd came from dup() below and is used nowhere else.
            unsafe {
                libc::dup2(fd, 2);
                libc::close(fd);
            }
        }
    }
}

/// Redirect stderr to a file; return the descriptor of the old stderr.
#[cfg(unix)]
fn redirect_stderr(dir: &std::path::Path) -> Option<i32> {
    use std::os::fd::AsRawFd;
    std::fs::create_dir_all(dir).ok()?;
    let file = std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(dir.join("stderr.log")).ok()?;
    // SAFETY: plain dup/dup2 calls on valid descriptors.
    unsafe {
        let saved = libc::dup(2);
        if saved < 0 {
            return None;
        }
        if libc::dup2(file.as_raw_fd(), 2) < 0 {
            libc::close(saved);
            return None;
        }
        Some(saved)
    }
}

async fn run(args: Args) -> anyhow::Result<Option<PathBuf>> {
    let paths = match &args.config_dir {
        Some(dir) => Paths::at(dir),
        None => Paths::system(),
    };
    // an explicit --room overrides the room baked into a friend's build
    let friend = BAKED_FRIEND == Some("1") && BAKED_ROOM.is_some() && args.room.is_none();
    let launch = Launch {
        version: VERSION.to_string(),
        room: args.room.clone().or_else(|| BAKED_ROOM.map(str::to_string)).unwrap_or_default(),
        friend,
        name: args.name.clone(),
        password: args.password.clone().or_else(|| BAKED_PASSWORD.filter(|_| args.room.is_none()).map(str::to_string)),
    };
    // copies of files handed to other apps last time (an image viewer…)
    let _ = std::fs::remove_dir_all(paths.cache.join("open"));
    let secret = tovuer_node::load_identity(&paths.identity_file())
        .map_err(|e| anyhow::anyhow!("cannot store this install's identity in {}: {e}", paths.config.display()))?;
    let setup = NodeSetup {
        version: VERSION.to_string(),
        secret,
        data_dir: paths.rooms_dir(),
        files_dir: paths.file_cache(),
        release_dir: Some(paths.release_dir()),
        network: tovuer_node::Network::Internet,
    };

    // The engine hands finished frames straight to the network. The network
    // task is created later (it needs the engine itself), so a cell links them.
    let link: Arc<OnceLock<NetHandle>> = Arc::new(OnceLock::new());
    let engine = Arc::new(Engine::new({
        let link = link.clone();
        move |packet: Packet| {
            if let Some(net) = link.get() {
                let flags = if packet.end { voice::FLAG_END } else { 0 };
                net.voice(voice::encode(flags, 0, packet.seq, &packet.data));
            }
        }
    }));
    let (net_tx, mut net_rx) = mpsc::unbounded_channel();
    let (task_tx, mut task_rx) = mpsc::unbounded_channel();
    let handle = net::spawn(net_tx, engine.clone(), setup);
    let _ = link.set(handle.clone());

    let mut app = App::new(launch, paths.clone(), engine, handle, task_tx);
    if let Some(mic) = args.mic {
        app.settings.mic = mic;
    }
    if let Some(spk) = args.spk {
        app.settings.spk = spk;
    }

    let mut tui = Tui::enter(app.settings.mouse, &paths.cache)?;
    // asking the terminal which graphics it has must happen before input is read
    app.graphics = transfer::detect_graphics();
    match &app.graphics {
        Some(g) => {
            let font = g.font_size();
            app.note(
                tovuer::app::Tone::Info,
                format!("pictures: {} graphics, a cell is {}×{} px", transfer::graphics_name(g.protocol_type()), font.width, font.height),
            );
        }
        None => app.note(tovuer::app::Tone::Info, "pictures: this terminal draws them with blocks — o in the viewer opens the original"),
    }
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(80));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let outcome: anyhow::Result<()> = async {
        loop {
            if app.dirty {
                app.dirty = false;
                tui.terminal.draw(|frame| ui::draw(frame, &mut app))?;
                if std::mem::take(&mut app.ui.repaint) {
                    tui.terminal.clear()?;
                    tui.terminal.draw(|frame| ui::draw(frame, &mut app))?;
                }
                app.fetch_wanted_thumbs();
            }
            for effect in std::mem::take(&mut app.effects) {
                tui.apply(effect);
            }
            if app.quit {
                return Ok(());
            }
            tokio::select! {
                event = events.next() => match event {
                    Some(Ok(Event::Key(key))) => app.on_key(key),
                    Some(Ok(Event::Paste(text))) => app.on_paste(text),
                    Some(Ok(Event::Mouse(mouse))) => app.on_mouse(mouse),
                    Some(Ok(Event::FocusGained)) => app.on_focus(true),
                    Some(Ok(Event::FocusLost)) => app.on_focus(false),
                    Some(Ok(Event::Resize(..))) => app.dirty = true,
                    Some(Err(e)) => return Err(e.into()),
                    // the terminal was closed
                    None => return Ok(()),
                },
                Some(event) = net_rx.recv() => app.on_session_event(event),
                Some(event) = task_rx.recv() => app.on_task(event),
                _ = tick.tick() => app.on_tick(),
            }
            // events often come in bursts (history, the member list) — handle them all, draw once
            while let Ok(event) = net_rx.try_recv() {
                app.on_session_event(event);
            }
            while let Ok(event) = task_rx.try_recv() {
                app.on_task(event);
            }
        }
    }
    .await;

    app.flush();
    app.net.shutdown();
    app.engine.stop();
    tui.leave();
    outcome?;
    Ok(app.restart.take())
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    #[cfg(windows)]
    transfer::cleanup_old_self();
    if args.list_devices {
        return list_devices();
    }
    if args.install {
        for line in tovuer::install::install()?.lines() {
            println!("{line}");
        }
        return Ok(());
    }

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let restart = runtime.block_on(run(args));
    // background tasks (audio, network) must not hold up exiting
    runtime.shutdown_timeout(Duration::from_millis(500));
    if let Some(exe) = restart? {
        let error = transfer::restart(&exe);
        anyhow::bail!("the update is downloaded but the restart failed ({error}) — start {} again", exe.display());
    }
    Ok(())
}
