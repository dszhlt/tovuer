//! Video in voice channels: our camera or screen goes out as small JPEG
//! frames, the frames of others are decoded away from the interface.
//!
//! Capturing needs no libraries linked in: a camera is read by ffmpeg (V4L2
//! on Linux, DirectShow on Windows, AVFoundation on macOS), a screen by grim
//! on Wayland (Hyprland, Sway and other wlroots desktops) and by ffmpeg
//! elsewhere. Both hand over JPEGs — exactly what goes over the wire.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bytes::Bytes;
use image::{DynamicImage, RgbImage};
use ratatui_image::picker::Capability;
use ratatui_image::protocol::Protocol;
use ratatui_image::protocol::kitty::Kitty;
use tovuer_proto::{VideoSource, video};

use crate::transfer::{Events, Graphics, Sharp, TaskEvent};

/// The device name for a test picture instead of a real camera or screen.
pub const TEST_DEVICE: &str = "test";

/// How a source is sent: the largest picture, frames a second, JPEG quality
/// (ffmpeg's scale: 2 — best, 31 — worst).
struct Profile {
    width: u32,
    height: u32,
    fps: u32,
    quality: u32,
}

const CAMERA: Profile = Profile { width: 480, height: 360, fps: 30, quality: 6 };
/// A screen: smooth — 45 frames a second — at a size that still shows text.
/// Frames that did not change are not sent, so a still screen costs nothing.
const SCREEN: Profile = Profile { width: 1440, height: 900, fps: 45, quality: 5 };
/// grim shots on their way at once: one takes about 25 ms, so a second one
/// started halfway keeps 45 frames a second going.
const GRIM_WORKERS: usize = 2;
/// A frame the same as the last one goes again only this often — for
/// whoever started watching in the meantime.
const SAME_AGAIN: Duration = Duration::from_secs(2);
/// grim's JPEG quality for a screen (its scale: 0–100).
const GRIM_QUALITY: &str = "60";

fn profile(source: VideoSource) -> &'static Profile {
    match source {
        VideoSource::Camera => &CAMERA,
        VideoSource::Screen => &SCREEN,
    }
}

pub fn source_name(source: VideoSource) -> &'static str {
    match source {
        VideoSource::Camera => "camera",
        VideoSource::Screen => "screen",
    }
}

/// A decoded frame.
pub struct VideoFrame {
    pub source: VideoSource,
    pub seq: u32,
    pub image: RgbImage,
}

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "VideoFrame({:?} #{} {}×{})", self.source, self.seq, self.image.width(), self.image.height())
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------ JPEG

enum Scan {
    /// Not all of it is here yet.
    More,
    /// Not a JPEG.
    Bad,
    /// The JPEG is this many bytes long.
    Done(usize),
}

/// Walk the JPEG at the start of `buf` segment by segment to find where it
/// ends: a JPEG stream has no lengths, only markers.
fn scan_jpeg(buf: &[u8]) -> Scan {
    if buf.len() < 2 {
        return Scan::More;
    }
    if buf[..2] != [0xff, 0xd8] {
        return Scan::Bad;
    }
    let mut i = 2;
    loop {
        if i > video::MAX_FRAME {
            return Scan::Bad;
        }
        let Some(&[ff, marker]) = buf.get(i..i + 2) else { return Scan::More };
        if ff != 0xff {
            return Scan::Bad;
        }
        match marker {
            0xff => i += 1, // fill
            0xd9 => return Scan::Done(i + 2),
            0x01 | 0xd0..=0xd7 => i += 2,
            _ => {
                let Some(&[hi, lo]) = buf.get(i + 2..i + 4) else { return Scan::More };
                let len = u16::from_be_bytes([hi, lo]) as usize;
                if len < 2 {
                    return Scan::Bad;
                }
                i += 2 + len;
                if marker == 0xda {
                    // the picture data runs to the next real marker
                    loop {
                        let Some(&[a, b]) = buf.get(i..i + 2) else { return Scan::More };
                        if a == 0xff && b != 0x00 && !(0xd0..=0xd7).contains(&b) && b != 0xff {
                            break;
                        }
                        i += if a == 0xff && b != 0xff { 2 } else { 1 };
                        if i > video::MAX_FRAME {
                            return Scan::Bad;
                        }
                    }
                }
            }
        }
    }
}

/// The picture size from a JPEG's frame header.
fn jpeg_size(jpeg: &[u8]) -> Option<(u16, u16)> {
    let mut i = 2;
    loop {
        let &[0xff, marker, hi, lo] = jpeg.get(i..i + 4)? else { return None };
        let len = u16::from_be_bytes([hi, lo]) as usize;
        if matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            let s = jpeg.get(i + 5..i + 9)?;
            return Some((u16::from_be_bytes([s[2], s[3]]), u16::from_be_bytes([s[0], s[1]])));
        }
        if marker == 0xda || len < 2 {
            return None;
        }
        i += 2 + len;
    }
}

/// Decode a JPEG, insisting on the size its sender claimed.
fn decode_jpeg(jpeg: &[u8], width: u16, height: u16) -> Option<RgbImage> {
    let mut reader = image::ImageReader::with_format(Cursor::new(jpeg), image::ImageFormat::Jpeg);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(video::MAX_WIDTH as u32);
    limits.max_image_height = Some(video::MAX_HEIGHT as u32);
    limits.max_alloc = Some(32 << 20);
    reader.limits(limits);
    let img = reader.decode().ok()?.into_rgb8();
    (img.width() == width as u32 && img.height() == height as u32).then_some(img)
}

/// A wire frame ([`video`] format) decoded.
pub fn decode(frame: &[u8]) -> Option<VideoFrame> {
    let f = video::parse(frame)?;
    let image = decode_jpeg(f.jpeg, f.width, f.height)?;
    Some(VideoFrame { source: f.source, seq: f.seq, image })
}

// ---------------------------------------------------------------- decoding

/// Frames waiting to be decoded (the newest of each person) and whether
/// the decoder is closed.
type Queue = (Mutex<(HashMap<u32, Bytes>, bool)>, Condvar);

/// Video frames decoded on a thread of their own. While it is busy only the
/// newest frame of each person waits; older ones are dropped.
#[derive(Clone)]
pub struct Decoder {
    shared: Arc<Queue>,
}

impl Decoder {
    /// For the frames of others; it lives as long as the program.
    pub fn spawn(events: Events) -> Self {
        Self::start(events, false)
    }

    /// For our own frames (the picture of what we show); [`Decoder::close`] ends it.
    fn own(events: Events) -> Self {
        Self::start(events, true)
    }

    fn start(events: Events, own: bool) -> Self {
        let shared: Arc<Queue> = Arc::default();
        let work = shared.clone();
        std::thread::Builder::new()
            .name("video-decode".into())
            .spawn(move || {
                loop {
                    let waiting = {
                        let (queue, ready) = &*work;
                        let mut queue = lock(queue);
                        while queue.0.is_empty() && !queue.1 {
                            queue = ready.wait(queue).unwrap_or_else(|e| e.into_inner());
                        }
                        if queue.0.is_empty() {
                            return; // closed
                        }
                        std::mem::take(&mut queue.0)
                    };
                    for (from, frame) in waiting {
                        let from = (!own).then_some(from);
                        if let Some(frame) = decode(&frame)
                            && events.send(TaskEvent::Video { from, frame: Arc::new(frame) }).is_err()
                        {
                            return; // the interface is gone
                        }
                    }
                }
            })
            .expect("cannot start the video decoder");
        Self { shared }
    }

    /// A frame from the network (already checked by the node).
    pub fn push(&self, from: u32, frame: Bytes) {
        let (queue, ready) = &*self.shared;
        lock(queue).0.insert(from, frame);
        ready.notify_one();
    }

    /// Let the thread go once it has nothing left.
    fn close(&self) {
        let (queue, ready) = &*self.shared;
        lock(queue).1 = true;
        ready.notify_one();
    }
}

// ---------------------------------------------------------------- devices

/// A camera: its name and what ffmpeg is given to open it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Camera {
    pub name: String,
    pub input: Vec<String>,
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

/// ffmpeg's list of devices (it prints them as errors).
#[cfg(not(target_os = "linux"))]
fn ffmpeg_devices(args: &[&str]) -> String {
    let out = Command::new("ffmpeg").args(["-hide_banner", "-nostdin"]).args(args).stdin(Stdio::null()).stdout(Stdio::null()).output();
    out.map(|o| String::from_utf8_lossy(&o.stderr).into_owned()).unwrap_or_default()
}

/// The cameras of this computer, the usual one first.
pub fn cameras() -> Vec<Camera> {
    #[cfg(target_os = "linux")]
    {
        // a webcam has several /dev/video nodes; the capture one has index 0
        let mut found: Vec<(u32, Camera)> = std::fs::read_dir("/sys/class/video4linux")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let node = entry.file_name().to_string_lossy().into_owned();
                let number: u32 = node.strip_prefix("video")?.parse().ok()?;
                let index = std::fs::read_to_string(entry.path().join("index")).unwrap_or_default();
                if index.trim().parse::<u32>().is_ok_and(|i| i != 0) {
                    return None;
                }
                let name = std::fs::read_to_string(entry.path().join("name")).unwrap_or_else(|_| node.clone());
                let input = vec!["-f".into(), "v4l2".into(), "-i".into(), format!("/dev/{node}")];
                Some((number, Camera { name: name.trim().to_string(), input }))
            })
            .collect();
        found.sort_by_key(|(n, _)| *n);
        found.into_iter().map(|(_, c)| c).collect()
    }
    #[cfg(windows)]
    {
        ffmpeg_devices(&["-list_devices", "true", "-f", "dshow", "-i", "dummy"])
            .lines()
            .filter(|l| l.trim_end().ends_with("(video)"))
            .filter_map(|l| Some(l.split('"').nth(1)?.to_string()))
            .map(|name| Camera { input: vec!["-f".into(), "dshow".into(), "-i".into(), format!("video={name}")], name })
            .collect()
    }
    #[cfg(target_os = "macos")]
    {
        avfoundation_list()
            .into_iter()
            .filter(|(_, name)| !name.starts_with("Capture screen"))
            .map(|(index, name)| Camera { input: strings(&["-f", "avfoundation", "-framerate", "30", "-i", &format!("{index}:none")]), name })
            .collect()
    }
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    {
        Vec::new()
    }
}

/// AVFoundation's video devices: `[1] FaceTime HD Camera`, `[2] Capture screen 0`.
#[cfg(target_os = "macos")]
fn avfoundation_list() -> Vec<(u32, String)> {
    let listing = ffmpeg_devices(&["-f", "avfoundation", "-list_devices", "true", "-i", ""]);
    let mut video = false;
    let mut found = Vec::new();
    for line in listing.lines() {
        if line.contains("video devices:") {
            video = true;
        } else if line.contains("audio devices:") {
            video = false;
        } else if video && let Some(rest) = line.rsplit_once("] [").map(|(_, r)| r) {
            if let Some((index, name)) = rest.split_once("] ") {
                if let Ok(index) = index.parse() {
                    found.push((index, name.trim().to_string()));
                }
            }
        }
    }
    found
}

/// The camera to open: `wanted` is part of its name, empty — the first one.
fn pick_camera(wanted: &str) -> Result<Vec<String>, String> {
    if wanted.eq_ignore_ascii_case(TEST_DEVICE) {
        return Ok(test_input(VideoSource::Camera));
    }
    let all = cameras();
    let lower = wanted.to_lowercase();
    let found = all.iter().find(|c| wanted.is_empty() || c.name.to_lowercase().contains(&lower) || c.input.iter().any(|a| *a == wanted));
    match found {
        Some(camera) => Ok(camera.input.clone()),
        None if all.is_empty() => Err("no camera found on this computer (/webcam test shows a test picture)".into()),
        None => Err(format!("no camera called “{wanted}” — /devices lists them")),
    }
}

fn test_input(source: VideoSource) -> Vec<String> {
    let p = profile(source);
    let size = match source {
        VideoSource::Camera => "640x480",
        VideoSource::Screen => "1280x720",
    };
    strings(&["-re", "-f", "lavfi", "-i", &format!("testsrc2=size={size}:rate={}", p.fps)])
}

/// How the screen is captured here.
enum ScreenGrab {
    /// grim in a loop (Wayland): the output to grab, or all of them.
    #[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
    Grim(Option<String>),
    /// An ffmpeg input.
    Ffmpeg(Vec<String>),
}

fn pick_screen(wanted: &str) -> Result<ScreenGrab, String> {
    if wanted.eq_ignore_ascii_case(TEST_DEVICE) {
        return Ok(ScreenGrab::Ffmpeg(test_input(VideoSource::Screen)));
    }
    let fps = SCREEN.fps.to_string();
    #[cfg(windows)]
    {
        let _ = wanted;
        Ok(ScreenGrab::Ffmpeg(strings(&["-f", "gdigrab", "-framerate", &fps, "-i", "desktop"])))
    }
    #[cfg(target_os = "macos")]
    {
        let screens: Vec<(u32, String)> = avfoundation_list().into_iter().filter(|(_, n)| n.starts_with("Capture screen")).collect();
        let lower = wanted.to_lowercase();
        let screen = screens.iter().find(|(_, n)| wanted.is_empty() || n.to_lowercase().contains(&lower)).ok_or("no screen to capture")?;
        Ok(ScreenGrab::Ffmpeg(strings(&["-f", "avfoundation", "-framerate", &fps, "-capture_cursor", "1", "-i", &format!("{}:none", screen.0)])))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            let output = if wanted.is_empty() { focused_output() } else { Some(wanted.to_string()) };
            return Ok(ScreenGrab::Grim(output));
        }
        match std::env::var("DISPLAY") {
            Ok(display) if !display.is_empty() => Ok(ScreenGrab::Ffmpeg(strings(&["-f", "x11grab", "-framerate", &fps, "-i", &display]))),
            _ => Err("no graphical session to capture".into()),
        }
    }
}

/// Hyprland's focused monitor: one screen, not all of them side by side.
#[cfg(not(any(windows, target_os = "macos")))]
fn focused_output() -> Option<String> {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let out = Command::new("hyprctl").args(["-j", "monitors"]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let monitors: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).ok()?;
    let focused = monitors.iter().find(|m| m["focused"].as_bool() == Some(true)).or(monitors.first())?;
    focused["name"].as_str().map(str::to_string)
}

// ---------------------------------------------------------------- capture

/// Our camera or screen being sent. Dropping it stops the capture.
pub struct Capture {
    pub source: VideoSource,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    /// Frames taken in the last second.
    rate: Arc<std::sync::atomic::AtomicU32>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(mut child) = lock(&self.child).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Capture {
    /// Frames taken a second (sent ones are fewer when nothing moves).
    pub fn fps(&self) -> u32 {
        self.rate.load(Ordering::Relaxed)
    }

    /// Start capturing. Every frame goes to `send` in the wire format and,
    /// decoded, to `events` as our own; a capture that ends by itself (no
    /// camera, ffmpeg missing) sends [`TaskEvent::VideoEnded`].
    pub fn start(source: VideoSource, device: &str, send: impl Fn(Bytes) + Send + 'static, events: Events) -> Capture {
        let stop = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let rate = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let capture = Capture { source, stop: stop.clone(), child: child.clone(), rate: rate.clone() };
        let device = device.to_string();
        let ended = events.clone();
        let thread = std::thread::Builder::new().name(format!("video-{}", source_name(source))).spawn(move || {
            let mut out = Outbox { source, seq: 0, send: Box::new(send), view: Decoder::own(events.clone()), last: None, rate, taken: (Instant::now(), 0) };
            let result = match source {
                VideoSource::Camera => pick_camera(&device).and_then(|input| stream(input, &CAMERA, &stop, &child, &mut out)),
                VideoSource::Screen => pick_screen(&device).and_then(|grab| match grab {
                    ScreenGrab::Ffmpeg(input) => stream(input, &SCREEN, &stop, &child, &mut out),
                    ScreenGrab::Grim(output) => grim_loop(output, &stop, &mut out),
                }),
            };
            if !stop.load(Ordering::Relaxed) {
                let _ = events.send(TaskEvent::VideoEnded { source, error: result.err() });
            }
        });
        if let Err(e) = thread {
            let _ = ended.send(TaskEvent::VideoEnded { source, error: Some(format!("cannot start the capture: {e}")) });
        }
        capture
    }
}

/// Where captured JPEGs go.
struct Outbox {
    source: VideoSource,
    seq: u32,
    send: Box<dyn Fn(Bytes) + Send>,
    /// Decodes our own frames for our tile, away from the capture.
    view: Decoder,
    /// The last frame sent: a fingerprint and when.
    last: Option<(u64, Instant)>,
    /// Frames taken per second, and the count since the second began.
    rate: Arc<std::sync::atomic::AtomicU32>,
    taken: (Instant, u32),
}

impl Outbox {
    fn deliver(&mut self, jpeg: &[u8]) {
        self.taken.1 += 1;
        let second = self.taken.0.elapsed();
        if second >= Duration::from_secs(1) {
            self.rate.store((self.taken.1 as f64 / second.as_secs_f64()).round() as u32, Ordering::Relaxed);
            self.taken = (Instant::now(), 0);
        }
        let Some((w, h)) = jpeg_size(jpeg) else { return };
        if w > video::MAX_WIDTH || h > video::MAX_HEIGHT || jpeg.len() + video::HEADER_LEN > video::MAX_FRAME {
            return;
        }
        // nothing moved: nothing to send (now and then it goes anyway)
        let print = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            jpeg.hash(&mut h);
            h.finish()
        };
        if self.last.is_some_and(|(p, at)| p == print && at.elapsed() < SAME_AGAIN) {
            return;
        }
        self.last = Some((print, Instant::now()));
        self.seq = self.seq.wrapping_add(1);
        let frame = Bytes::from(video::encode(self.source, self.seq, w, h, jpeg));
        (self.send)(frame.clone());
        self.view.push(0, frame);
    }
}

impl Drop for Outbox {
    fn drop(&mut self) {
        self.view.close();
    }
}

fn missing(tool: &str, what: &str) -> String {
    format!("{what} needs {tool} — install it and try again")
}

/// The last lines a tool printed to stderr: what to tell the person.
fn last_words(stderr: &str, fallback: &str) -> String {
    let line = stderr.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or(fallback);
    tovuer_proto::one_line(line, 160)
}

/// ffmpeg turns the input into a stream of JPEGs on its stdout.
fn stream(input: Vec<String>, p: &Profile, stop: &AtomicBool, slot: &Mutex<Option<Child>>, out: &mut Outbox) -> Result<(), String> {
    let filter = format!(
        "scale=w={}:h={}:force_original_aspect_ratio=decrease:force_divisible_by=2,format=yuvj420p",
        p.width, p.height
    );
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(&input)
        .args(["-an", "-vf", &filter, "-r", &p.fps.to_string(), "-c:v", "mjpeg", "-q:v", &p.quality.to_string(), "-f", "mjpeg", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => missing("ffmpeg", "video"),
        _ => format!("cannot start ffmpeg: {e}"),
    })?;
    let mut stdout = child.stdout.take().ok_or("no output from ffmpeg")?;
    let stderr = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = err.read_to_string(&mut text);
            text
        })
    });
    *lock(slot) = Some(child);
    if stop.load(Ordering::Relaxed) {
        // stopped while starting: the drop that should have killed it came too early
        if let Some(mut child) = lock(slot).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        return Ok(());
    }

    let mut buf: Vec<u8> = Vec::with_capacity(256 << 10);
    let mut chunk = vec![0u8; 64 << 10];
    let mut frames = 0u64;
    loop {
        let n = match stdout.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        loop {
            match scan_jpeg(&buf) {
                Scan::More => break,
                Scan::Done(len) => {
                    out.deliver(&buf[..len]);
                    frames += 1;
                    buf.drain(..len);
                }
                Scan::Bad => {
                    // lost the beginning of a picture: skip to the next one
                    match buf.windows(2).skip(1).position(|w| w == [0xff, 0xd8]) {
                        Some(at) => drop(buf.drain(..at + 1)),
                        None => buf.clear(),
                    }
                }
            }
        }
    }
    if let Some(mut child) = lock(slot).take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    let said = stderr.and_then(|t| t.join().ok()).unwrap_or_default();
    let fallback = if frames == 0 { "the device gave no picture" } else { "the picture stopped" };
    Err(last_words(&said, fallback))
}

/// Wayland: grim screenshots, two on their way at once, one started every
/// 1/45 s; each is waited for on a thread of its own and frames go out in
/// the order they were taken. grim scales the picture itself — quickly down
/// to about 3/4, several times slower below that — so the screen is taken
/// as large as a frame may be: full size up to 1440×900, a bit smaller above.
fn grim_loop(output: Option<String>, stop: &AtomicBool, out: &mut Outbox) -> Result<(), String> {
    use std::sync::mpsc::{RecvTimeoutError, channel};
    let every = Duration::from_micros(1_000_000 / SCREEN.fps as u64);
    // (which shot, the scale it was taken at, what grim gave)
    let (done, results) = channel::<(u64, f64, std::io::Result<std::process::Output>)>();
    let mut scale = 1.0f64;
    let (mut started, mut shown, mut on_the_way) = (0u64, 0u64, 0usize);
    let mut ready = std::collections::BTreeMap::new();
    let mut next = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if on_the_way < GRIM_WORKERS && Instant::now() >= next {
            let mut cmd = Command::new("grim");
            cmd.args(["-c", "-t", "jpeg", "-q", GRIM_QUALITY, "-s", &format!("{scale:.3}")]);
            if let Some(name) = &output {
                cmd.args(["-o", name]);
            }
            cmd.arg("-").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
            let child = cmd.spawn().map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => missing("grim", "screen sharing on Wayland"),
                _ => format!("cannot start grim: {e}"),
            })?;
            let (done, index, taken_at) = (done.clone(), started, scale);
            std::thread::spawn(move || {
                let _ = done.send((index, taken_at, child.wait_with_output()));
            });
            started += 1;
            on_the_way += 1;
            // late? go on from now rather than catch up in a burst
            next = (next + every).max(Instant::now());
        }
        let wait = if on_the_way < GRIM_WORKERS { next.saturating_duration_since(Instant::now()) } else { Duration::from_secs(2) };
        match results.recv_timeout(wait) {
            Ok((index, taken_at, shot)) => {
                on_the_way -= 1;
                ready.insert(index, (taken_at, shot));
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // in the order they were taken
        while let Some((taken_at, shot)) = ready.remove(&shown) {
            shown += 1;
            let shot = shot.map_err(|e| e.to_string())?;
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            if !shot.status.success() {
                return Err(last_words(&String::from_utf8_lossy(&shot.stderr), "grim could not capture the screen"));
            }
            if let Some((w, h)) = jpeg_size(&shot.stdout) {
                // from the scale this shot was taken at: shots on their way
                // were started before the last change
                scale = grim_scale(taken_at, w, h);
                // a frame over the limits is dropped; the next one fits
                out.deliver(&shot.stdout);
            }
        }
    }
    Ok(())
}

/// The next scale for grim after a shot of `w`×`h` taken at `scale`: the
/// largest that keeps the screen within the frame size, never above 1.
fn grim_scale(scale: f64, w: u16, h: u16) -> f64 {
    let fit = (SCREEN.width as f64 / w.max(1) as f64).min(SCREEN.height as f64 / h.max(1) as f64);
    if fit < 0.98 || (fit > 1.02 && scale < 1.0) { (scale * fit).clamp(0.05, 1.0) } else { scale }
}

// ---------------------------------------------------------------- drawing

/// A Kitty image id for each tile: a new frame replaces the old picture in
/// the terminal instead of piling up next to it.
pub fn kitty_id(user: u32) -> u32 {
    0x7a00_0000 | (user & 0x00ff_ffff)
}

/// A frame for the terminal's own graphics: fitted into exactly
/// `cols`×`rows` cells in the middle of `backdrop`. Sixel and iTerm2 get
/// every pixel of the cells; Kitty gets no more pixels than the frame has
/// and stretches them over the cells itself.
pub fn encode_sharp(img: &RgbImage, mirror: bool, graphics: &Graphics, cols: u16, rows: u16, backdrop: [u8; 3], id: u32) -> Result<Sharp, String> {
    let font = graphics.font_size();
    let (cols, rows) = (cols.max(1), rows.max(1));
    let (box_w, box_h) = (cols as u32 * font.width.max(1) as u32, rows as u32 * font.height.max(1) as u32);
    let kind = graphics.protocol_type();
    let kitty = kind == ratatui_image::picker::ProtocolType::Kitty;
    let (fit_w, fit_h) = fit(img.width(), img.height(), box_w, box_h);
    let shrink = if kitty { (img.width() as f64 / fit_w as f64).max(img.height() as f64 / fit_h as f64).min(1.0) } else { 1.0 };
    let (canvas_w, canvas_h) = ((box_w as f64 * shrink).round().max(1.0) as u32, (box_h as f64 * shrink).round().max(1.0) as u32);
    let (w, h) = fit(img.width(), img.height(), canvas_w, canvas_h);
    let mut frame = image::imageops::resize(img, w, h, image::imageops::FilterType::Triangle);
    if mirror {
        image::imageops::flip_horizontal_in_place(&mut frame);
    }
    let mut canvas = RgbImage::from_pixel(canvas_w, canvas_h, image::Rgb(backdrop));
    image::imageops::overlay(&mut canvas, &frame, ((canvas_w - w) / 2) as i64, ((canvas_h - h) / 2) as i64);
    let size = ratatui::layout::Size::new(cols, rows);
    let proto = if kitty {
        let compress = graphics.capabilities().contains(&Capability::KittyCompression);
        Protocol::Kitty(Kitty::new(DynamicImage::ImageRgb8(canvas), size, id, graphics.tmux_detected(), compress).map_err(|e| e.to_string())?)
    } else {
        graphics
            .new_protocol(DynamicImage::ImageRgb8(canvas), size, ratatui_image::Resize::Fit(None))
            .map_err(|e| e.to_string())?
    };
    Ok(Sharp { proto, kind, stretch: kitty && shrink < 1.0 })
}

/// The largest `w`×`h` with the picture's proportions inside `box_w`×`box_h`.
pub fn fit(img_w: u32, img_h: u32, box_w: u32, box_h: u32) -> (u32, u32) {
    let (img_w, img_h) = (img_w.max(1) as u64, img_h.max(1) as u64);
    let (box_w, box_h) = (box_w.max(1) as u64, box_h.max(1) as u64);
    if box_w * img_h <= box_h * img_w {
        (box_w as u32, ((box_w * img_h / img_w).max(1)) as u32)
    } else {
        (((box_h * img_w / img_h).max(1)) as u32, box_h as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 7) as u8, (y * 5) as u8, 90]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80).encode_image(&img).unwrap();
        out
    }

    #[test]
    fn a_stream_of_jpegs_splits_into_pictures() {
        let (a, b) = (jpeg(40, 30), jpeg(16, 12));
        let mut stream = a.clone();
        stream.extend_from_slice(&b);
        let Scan::Done(len) = scan_jpeg(&stream) else { panic!("the first picture was not found") };
        assert_eq!(len, a.len());
        assert!(matches!(scan_jpeg(&stream[len..]), Scan::Done(n) if n == b.len()));
        assert!(matches!(scan_jpeg(&a[..a.len() - 1]), Scan::More), "half a picture waits for the rest");
        assert!(matches!(scan_jpeg(b"\x89PNG"), Scan::Bad));
        assert_eq!(jpeg_size(&a), Some((40, 30)));
        assert_eq!(jpeg_size(&b), Some((16, 12)));
    }

    #[test]
    fn frames_decode_only_at_the_size_they_claim() {
        let picture = jpeg(40, 30);
        let frame = video::encode(VideoSource::Camera, 3, 40, 30, &picture);
        let decoded = decode(&frame).unwrap();
        assert_eq!((decoded.seq, decoded.image.width(), decoded.image.height()), (3, 40, 30));
        assert!(decode(&video::encode(VideoSource::Camera, 3, 41, 30, &picture)).is_none(), "a lying header");
        assert!(decode(&video::encode(VideoSource::Camera, 3, 40, 30, &[0xff, 0xd8, 0, 0, 0xff, 0xd9])).is_none());
    }

    #[test]
    fn grim_takes_the_screen_as_large_as_a_frame_may_be() {
        // 1920×1080 at full size first, then 3/4: 1440×810, still in grim's quick range
        let s = grim_scale(1.0, 1920, 1080);
        assert!((s - 0.75).abs() < 0.001, "{s}");
        assert_eq!(grim_scale(s, 1440, 810), s, "settled");
        assert_eq!(grim_scale(1.0, 1280, 800), 1.0, "a small screen is taken whole");
        assert!((grim_scale(1.0, 3840, 2160) - 0.375).abs() < 0.001, "4K goes down to the limit");
        // a late full-size shot (taken before the change) gives the same answer, not a smaller one
        assert!((grim_scale(1.0, 1920, 1080) - grim_scale(1.0, 1920, 1080)).abs() < 1e-9);
        assert_eq!(grim_scale(0.75, 1440, 810), 0.75, "settled shots keep it");
    }

    #[test]
    fn a_frame_that_did_not_change_is_not_sent_again_at_once() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let sent = Arc::new(Mutex::new(0));
        let counter = sent.clone();
        let rate = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut out = Outbox { source: VideoSource::Screen, seq: 0, send: Box::new(move |_| *lock(&counter) += 1), view: Decoder::own(tx), last: None, rate, taken: (Instant::now(), 0) };
        let (a, b) = (jpeg(16, 12), jpeg(20, 12));
        out.deliver(&a);
        out.deliver(&a);
        out.deliver(&a);
        assert_eq!(*lock(&sent), 1, "the same screen goes once");
        out.deliver(&b);
        assert_eq!((*lock(&sent), out.seq), (2, 2), "a change goes at once");
        out.last = Some((out.last.unwrap().0, Instant::now() - SAME_AGAIN));
        out.deliver(&b);
        assert_eq!(*lock(&sent), 3, "and a still one now and then");
    }

    #[test]
    fn pictures_fit_inside_keeping_their_shape() {
        assert_eq!(fit(480, 360, 100, 100), (100, 75));
        assert_eq!(fit(480, 360, 400, 150), (200, 150));
        assert_eq!(fit(1, 1000, 10, 10), (1, 10));
    }

    #[test]
    fn the_decoder_hands_over_the_newest_frame() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let decoder = Decoder::spawn(tx);
        decoder.push(5, Bytes::from(video::encode(VideoSource::Screen, 9, 16, 12, &jpeg(16, 12))));
        decoder.push(6, Bytes::from_static(b"junk"));
        let got = rx.blocking_recv().unwrap();
        let TaskEvent::Video { from, frame } = got else { panic!("{got:?}") };
        assert_eq!((from, frame.seq, frame.source), (Some(5), 9, VideoSource::Screen));
    }

    /// ffmpeg's test picture through the real capture path.
    #[test]
    fn the_test_camera_sends_frames() {
        if Command::new("ffmpeg").arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_err() {
            eprintln!("no ffmpeg — skipped");
            return;
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let capture = {
            let sent = sent.clone();
            Capture::start(VideoSource::Camera, TEST_DEVICE, move |f| lock(&sent).push(f), tx)
        };
        let got = rx.blocking_recv().unwrap();
        let TaskEvent::Video { from: None, frame } = got else { panic!("{got:?}") };
        assert_eq!((frame.image.width(), frame.image.height()), (480, 360));
        drop(capture);
        let wire = lock(&sent)[0].clone();
        let parsed = video::parse(&wire).unwrap();
        assert_eq!((parsed.source, parsed.width, parsed.height), (VideoSource::Camera, 480, 360));
    }
}
