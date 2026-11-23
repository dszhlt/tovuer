//! Background work: sharing and fetching files through the node, pictures
//! for previews, self-update, desktop notifications. Results come back to
//! the interface as [`TaskEvent`]s.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::DynamicImage;
use ratatui_image::picker::{Picker as Graphics, ProtocolType};
use ratatui_image::protocol::Protocol;
use tokio::sync::mpsc;
use tovuer_node::Node;
use tovuer_proto::FileMeta;
use tovuer_voice::{DeviceInfo, StreamInfo};

use crate::settings::{Paths, download_dir, free_path};

/// Thumbnail size in the chat: at most this many cells.
pub const THUMB_COLS: u16 = 36;
pub const THUMB_ROWS: u16 = 9;
/// Bigger pictures are not fetched automatically for a preview.
pub const AUTO_PREVIEW_LIMIT: u64 = 8 << 20;
/// Pictures are not kept bigger than this — a terminal cannot show more pixels.
const PICTURE_MAX_SIDE: u32 = 900;

pub type Events = mpsc::UnboundedSender<TaskEvent>;

#[derive(Debug)]
pub enum TaskEvent {
    Upload { id: u64, sent: u64, total: u64 },
    UploadDone { id: u64, name: String, error: Option<String> },
    Download { file_id: String, got: u64, total: u64 },
    Saved { name: String, result: Result<PathBuf, String> },
    Thumb { file_id: String, result: Result<Thumb, String> },
    Picture { file_id: String, name: String, result: Result<Arc<Picture>, String> },
    /// The picture encoded for the terminal's own graphics, for a viewer of `area` cells.
    Sharp { file_id: String, area: (u16, u16), result: Result<Arc<Sharp>, String> },
    /// A file handed to the system's own app (an image viewer, a PDF reader…).
    Opened { name: String, result: Result<(), String> },
    /// Preview of the picture under the file picker's cursor.
    Peek { path: PathBuf, result: Result<Thumb, String> },
    UpdateProgress { got: u64, total: u64 },
    /// `Ok(None)` — already the latest; `Ok(Some((exe, version)))` — replaced, time to restart.
    UpdateDone(Result<Option<(PathBuf, String)>, String>),
    Voice(Result<StreamInfo, String>),
    Devices(Result<(Vec<DeviceInfo>, Vec<DeviceInfo>), String>),
}

/// Backdrop behind transparent parts of a picture.
const BACKDROP: [u32; 3] = [24, 24, 28];

/// A picture in memory (RGB, transparency already blended onto a dark backdrop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub w: u32,
    pub h: u32,
    pub rgb: Vec<u8>,
    /// The size of the original, before it was scaled down for the screen.
    pub full: (u32, u32),
}

/// A picture shrunk to cells: each text line is two pixel rows ("▀").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumb {
    pub w: u16,
    /// Height in pixel rows, always even.
    pub h: u16,
    pub px: Vec<[u8; 3]>,
}

impl Thumb {
    /// How many text lines it takes.
    pub fn rows(&self) -> u16 {
        self.h / 2
    }

    pub fn at(&self, x: u16, y: u16) -> [u8; 3] {
        self.px[y as usize * self.w as usize + x as usize]
    }
}

/// Decode PNG/JPEG/GIF/WebP/BMP at full size, turned upright the way the
/// camera recorded it (phone photos are often stored sideways plus an EXIF
/// "rotate me" mark).
pub fn decode_image(bytes: &[u8]) -> Result<DynamicImage, String> {
    use image::ImageDecoder;
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("cannot read: {e}"))?;
    // protection from "bombs": a small file that unpacks into gigabytes
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(20_000);
    limits.max_image_height = Some(20_000);
    limits.max_alloc = Some(384 << 20);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|e| format!("unsupported format: {e}"))?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| format!("unsupported format: {e}"))?;
    img.apply_orientation(orientation);
    if img.width() == 0 || img.height() == 0 {
        return Err("an empty picture".into());
    }
    Ok(img)
}

/// Blend a pixel's transparency onto `backdrop`.
fn blend(p: image::Rgba<u8>, backdrop: [u8; 3]) -> [u8; 3] {
    let a = p[3] as u32;
    std::array::from_fn(|c| ((p[c] as u32 * a + backdrop[c] as u32 * (255 - a)) / 255) as u8)
}

impl Picture {
    /// Decode a picture for drawing with blocks. Huge pictures are scaled down right away.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let img = decode_image(bytes)?;
        let full = (img.width(), img.height());
        let img = if img.width() > PICTURE_MAX_SIDE || img.height() > PICTURE_MAX_SIDE {
            img.thumbnail(PICTURE_MAX_SIDE, PICTURE_MAX_SIDE)
        } else {
            img
        };
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        let backdrop = BACKDROP.map(|c| c as u8);
        let rgb = rgba.pixels().flat_map(|p| blend(*p, backdrop)).collect();
        Ok(Self { w, h, rgb, full })
    }

    /// Fit into `cols`×`rows` cells keeping the proportions (averaging blocks).
    pub fn fit(&self, cols: u16, rows: u16) -> Thumb {
        let (max_w, max_h) = (cols.max(1) as f64, rows.max(1) as f64 * 2.0);
        let scale = (max_w / self.w as f64).min(max_h / self.h as f64).min(1.0);
        let w = ((self.w as f64 * scale).round() as u32).clamp(1, cols.max(1) as u32);
        let mut h = ((self.h as f64 * scale).round() as u32).clamp(1, rows.max(1) as u32 * 2);
        h += h % 2;
        let mut px = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            let y0 = (y as u64 * self.h as u64 / h as u64) as u32;
            let y1 = (((y + 1) as u64 * self.h as u64 / h as u64) as u32).clamp(y0 + 1, self.h.max(y0 + 1));
            for x in 0..w {
                let x0 = (x as u64 * self.w as u64 / w as u64) as u32;
                let x1 = (((x + 1) as u64 * self.w as u64 / w as u64) as u32).clamp(x0 + 1, self.w.max(x0 + 1));
                let mut sum = [0u64; 3];
                let mut n = 0u64;
                for sy in y0..y1.min(self.h) {
                    for sx in x0..x1.min(self.w) {
                        let i = ((sy * self.w + sx) * 3) as usize;
                        for (acc, value) in sum.iter_mut().zip(&self.rgb[i..i + 3]) {
                            *acc += *value as u64;
                        }
                        n += 1;
                    }
                }
                // a row past the bottom edge (padding to an even height) is backdrop
                let channel = |c: usize| sum[c].checked_div(n).map_or(BACKDROP[c] as u8, |v| v as u8);
                px.push([channel(0), channel(1), channel(2)]);
            }
        }
        Thumb { w: w as u16, h: h as u16, px }
    }
}

/// A picture encoded for the terminal's own graphics (Sixel, Kitty, iTerm2):
/// real pixels instead of two colored blocks per character cell.
pub struct Sharp {
    pub proto: Protocol,
    pub kind: ProtocolType,
}

impl std::fmt::Debug for Sharp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sharp({:?}, {:?})", self.kind, self.proto.size())
    }
}

impl Sharp {
    /// How many cells it covers.
    pub fn size(&self) -> (u16, u16) {
        let size = self.proto.size();
        (size.width, size.height)
    }

    /// Whether the terminal keeps the picture only until text is drawn over
    /// it (Sixel, iTerm2) — then anything drawn on top means a full repaint.
    pub fn fragile(&self) -> bool {
        !matches!(self.kind, ProtocolType::Kitty)
    }

    /// Fit `img` into `cols`×`rows` cells at the terminal's real resolution
    /// (never enlarged), on `backdrop`, and encode it.
    pub fn encode(img: &DynamicImage, graphics: &Graphics, cols: u16, rows: u16, backdrop: [u8; 3]) -> Result<Self, String> {
        let font = graphics.font_size();
        let (fw, fh) = (font.width.max(1) as u32, font.height.max(1) as u32);
        let (box_w, box_h) = (cols.max(1) as u32 * fw, rows.max(1) as u32 * fh);
        let fitted;
        let img = if img.width() > box_w || img.height() > box_h {
            fitted = img.resize(box_w, box_h, image::imageops::FilterType::CatmullRom);
            &fitted
        } else {
            img
        };
        // a canvas of whole cells, the picture in its middle: the terminal
        // then shows it pixel for pixel, with no second resize
        let (w, h) = (img.width(), img.height());
        let (cells_w, cells_h) = (w.div_ceil(fw), h.div_ceil(fh));
        let mut canvas = image::RgbImage::from_pixel(cells_w * fw, cells_h * fh, image::Rgb(backdrop));
        let (dx, dy) = ((canvas.width() - w) / 2, (canvas.height() - h) / 2);
        for (x, y, p) in img.to_rgba8().enumerate_pixels() {
            canvas.put_pixel(x + dx, y + dy, image::Rgb(blend(*p, backdrop)));
        }
        let size = ratatui::layout::Size::new(cells_w as u16, cells_h as u16);
        let proto = graphics
            .new_protocol(DynamicImage::ImageRgb8(canvas), size, ratatui_image::Resize::Fit(None))
            .map_err(|e| format!("cannot draw the picture: {e}"))?;
        Ok(Self { proto, kind: graphics.protocol_type() })
    }
}

/// Which graphics the terminal can draw pictures with; `None` — only blocks.
/// Asks the terminal, so it must run in raw mode before input is read.
/// `TOVUER_GRAPHICS=off|sixel|kitty|iterm2` overrides the guess.
pub fn detect_graphics() -> Option<Graphics> {
    let wanted = std::env::var("TOVUER_GRAPHICS").unwrap_or_default().to_lowercase();
    if matches!(wanted.as_str(), "off" | "no" | "0" | "blocks" | "halfblocks") {
        return None;
    }
    // Windows consoles do not reliably answer the query, and an unanswered
    // query keeps eating keystrokes — there pictures stay blocks.
    if cfg!(windows) && wanted.is_empty() {
        return None;
    }
    let mut graphics = Graphics::from_query_stdio().ok()?;
    match wanted.as_str() {
        "sixel" => graphics.set_protocol_type(ProtocolType::Sixel),
        "kitty" => graphics.set_protocol_type(ProtocolType::Kitty),
        "iterm2" | "iterm" => graphics.set_protocol_type(ProtocolType::Iterm2),
        _ => {}
    }
    (graphics.protocol_type() != ProtocolType::Halfblocks).then_some(graphics)
}

/// The name of a graphics protocol, for settings and the log.
pub fn graphics_name(kind: ProtocolType) -> &'static str {
    match kind {
        ProtocolType::Sixel => "sixel",
        ProtocolType::Kitty => "kitty",
        ProtocolType::Iterm2 => "iterm2",
        ProtocolType::Halfblocks => "blocks",
    }
}

/// Open a file with the system's own app for its type.
pub fn open_with_system(path: &Path) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let mut command = if cfg!(windows) {
        let mut c = Command::new("explorer");
        c.arg(path);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(path);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(path);
        c
    };
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| if cfg!(any(windows, target_os = "macos")) { format!("cannot open: {e}") } else { format!("cannot run xdg-open: {e}") })?;
    // no zombies: wait for it on the side
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// A file name every system accepts (Windows forbids a few characters).
fn portable_name(name: &str) -> String {
    let clean: String = name.chars().map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() { '_' } else { c }).collect();
    let clean = clean.trim_end_matches(['.', ' ']).to_string();
    if clean.is_empty() { "file".into() } else { clean }
}

fn spawn_err(e: tokio::task::JoinError) -> String {
    format!("internal error: {e}")
}

async fn load_picture(path: PathBuf) -> Result<Picture, String> {
    tokio::task::spawn_blocking(move || {
        let bytes = std::fs::read(&path).map_err(|e| format!("cannot read: {e}"))?;
        Picture::decode(&bytes)
    })
    .await
    .map_err(spawn_err)?
}

/// File transfers of one session: they go through the local node, which
/// gets the bytes from whichever member has them.
#[derive(Clone)]
pub struct Remote {
    pub node: Node,
    pub events: Events,
    pub paths: Paths,
}

impl Remote {
    /// The file in the local store; fetched from the room if it is not there yet.
    async fn cached(&self, file: &FileMeta) -> Result<PathBuf, String> {
        let (events, id) = (self.events.clone(), file.id.clone());
        self.node
            .fetch_file(file, move |got, total| {
                let _ = events.send(TaskEvent::Download { file_id: id.clone(), got, total });
            })
            .await
    }

    /// Share a file in a channel. `id` numbers the upload for its progress bar.
    pub fn upload(&self, id: u64, path: PathBuf, channel: String, text: String, reply_to: Option<u64>) {
        let me = self.clone();
        tokio::spawn(async move {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            let events = me.events.clone();
            let error = me
                .node
                .upload(&path, &channel, &text, reply_to, move |sent, total| {
                    let _ = events.send(TaskEvent::Upload { id, sent, total });
                })
                .await
                .err();
            let _ = me.events.send(TaskEvent::UploadDone { id, name, error });
        });
    }

    /// Save a file to Downloads (or to `target`, if given).
    pub fn save(&self, file: FileMeta, target: Option<PathBuf>) {
        let me = self.clone();
        tokio::spawn(async move {
            let result = me.save_inner(&file, target).await;
            let _ = me.events.send(TaskEvent::Saved { name: file.name, result });
        });
    }

    async fn save_inner(&self, file: &FileMeta, target: Option<PathBuf>) -> Result<PathBuf, String> {
        let dest = match target {
            Some(t) if t.is_dir() => free_path(&t, &file.name),
            Some(t) => t,
            None => free_path(&download_dir(), &file.name),
        };
        let cached = self.cached(file).await?;
        if let Some(dir) = dest.parent() {
            tokio::fs::create_dir_all(dir).await.map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        tokio::fs::copy(&cached, &dest).await.map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
        Ok(dest)
    }

    /// Prepare a picture's thumbnail for the chat feed.
    pub fn thumb(&self, file: FileMeta) {
        let me = self.clone();
        tokio::spawn(async move {
            let result = async {
                let path = me.cached(&file).await?;
                Ok(load_picture(path).await?.fit(THUMB_COLS, THUMB_ROWS))
            }
            .await;
            let _ = me.events.send(TaskEvent::Thumb { file_id: file.id, result });
        });
    }

    /// Load a whole picture for the full-screen viewer.
    pub fn picture(&self, file: FileMeta) {
        let me = self.clone();
        tokio::spawn(async move {
            let result = async {
                let path = me.cached(&file).await?;
                Ok(Arc::new(load_picture(path).await?))
            }
            .await;
            let _ = me.events.send(TaskEvent::Picture { file_id: file.id, name: file.name, result });
        });
    }

    /// Encode a picture at full resolution for the terminal's graphics, to
    /// fill a viewer of `area` cells.
    pub fn sharp(&self, file: FileMeta, graphics: Graphics, area: (u16, u16), backdrop: [u8; 3]) {
        let me = self.clone();
        tokio::spawn(async move {
            let result = async {
                let path = me.cached(&file).await?;
                tokio::task::spawn_blocking(move || {
                    let bytes = std::fs::read(&path).map_err(|e| format!("cannot read: {e}"))?;
                    let img = decode_image(&bytes)?;
                    Sharp::encode(&img, &graphics, area.0, area.1, backdrop).map(Arc::new)
                })
                .await
                .map_err(spawn_err)?
            }
            .await;
            let _ = me.events.send(TaskEvent::Sharp { file_id: file.id, area, result });
        });
    }

    /// Open the original file with the system's app (an image viewer for a
    /// picture), fetching it first if needed.
    pub fn open_outside(&self, file: FileMeta) {
        let me = self.clone();
        tokio::spawn(async move {
            let result = async {
                let cached = me.cached(&file).await?;
                // the store names files by hash; the app wants the real name
                // (and its extension) to know what it is
                let dir = me.paths.cache.join("open").join(&file.id[..file.id.len().min(12)]);
                let path = dir.join(portable_name(&file.name));
                if !path.exists() {
                    tokio::fs::create_dir_all(&dir).await.map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
                    if std::fs::hard_link(&cached, &path).is_err() {
                        tokio::fs::copy(&cached, &path).await.map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                    }
                }
                open_with_system(&path)
            }
            .await;
            let _ = me.events.send(TaskEvent::Opened { name: file.name, result });
        });
    }
}

/// Preview of a local picture (for the file picker).
pub fn peek(events: Events, path: PathBuf, cols: u16, rows: u16) {
    tokio::spawn(async move {
        let result = load_picture(path.clone()).await.map(|p| p.fit(cols, rows));
        let _ = events.send(TaskEvent::Peek { path, result });
    });
}

// ---------------------------------------------------------------- update

/// Update file name for this system: `tovuer-linux-amd64`, `tovuer-windows-amd64.exe`.
pub fn asset_name() -> String {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("tovuer-{}-{arch}{}", std::env::consts::OS, std::env::consts::EXE_SUFFIX)
}

/// Does the download look like an executable for this system. Never overwrite
/// a working client with something else (an error page, a truncated file).
pub fn looks_like_executable(data: &[u8]) -> bool {
    if data.len() < 256 * 1024 {
        return false;
    }
    if cfg!(windows) { data.starts_with(b"MZ") } else { data.starts_with(b"\x7fELF") || data.starts_with(&[0xcf, 0xfa, 0xed, 0xfe]) }
}

/// Write a new binary over the current one.
pub fn replace_self(data: &[u8]) -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    put_binary(&exe, data)?;
    Ok(exe)
}

/// Put a binary at `exe`, replacing what is there, even while it runs.
/// On Unix — an atomic swap; on Windows a running exe cannot be overwritten
/// but can be renamed.
pub fn put_binary(exe: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = exe.parent().unwrap_or(Path::new("."));
    let nonce = tovuer_node::util::hex(&tovuer_node::util::random::<8>());
    let tmp = dir.join(format!(".tovuer-update-{nonce}.tmp"));
    let mut staged = std::fs::File::create_new(&tmp)?;
    let result = (|| {
        std::io::Write::write_all(&mut staged, data)?;
        staged.sync_all()?;
        drop(staged);
        install_staged(&tmp, exe)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn install_staged(tmp: &Path, exe: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(windows)]
    if exe.exists() {
        let backup = update_backup_dir(exe);
        clean_update_backup(&backup);
        // Claim our own directory. An existing file or directory belongs to
        // someone else or to an unfinished update and must be preserved.
        std::fs::create_dir(&backup)?;
        let old = backup.join("previous.exe");
        if let Err(e) = std::fs::rename(exe, &old) {
            let _ = std::fs::remove_dir(&backup);
            return Err(e);
        }
        if let Err(e) = std::fs::rename(tmp, exe) {
            std::fs::rename(&old, exe)?;
            let _ = std::fs::remove_dir(&backup);
            return Err(e);
        }
        // A crash before the commit marker leaves the recovery copy intact.
        if std::fs::write(backup.join("committed"), UPDATE_COMMITTED).is_ok() {
            clean_update_backup(&backup);
        }
        return Ok(());
    }
    std::fs::rename(tmp, exe)
}

#[cfg(windows)]
fn update_backup_dir(exe: &Path) -> PathBuf {
    let mut name = exe.as_os_str().to_owned();
    name.push(".tovuer-update");
    name.into()
}

#[cfg(any(windows, test))]
const UPDATE_COMMITTED: &[u8] = b"tovuer: executable replacement completed\n";

#[cfg(any(windows, test))]
fn clean_update_backup(dir: &Path) {
    let marker = dir.join("committed");
    if !dir.symlink_metadata().is_ok_and(|m| m.file_type().is_dir())
        || std::fs::read(&marker).ok().as_deref() != Some(UPDATE_COMMITTED)
    {
        return;
    }
    match std::fs::remove_file(dir.join("previous.exe")) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return,
        _ => {}
    }
    let _ = std::fs::remove_file(marker);
    let _ = std::fs::remove_dir(dir);
}

#[cfg(windows)]
pub fn cleanup_old_self() {
    if let Ok(exe) = std::env::current_exe() {
        clean_update_backup(&update_backup_dir(&exe));
    }
}

/// Start the new binary in place of this process. Returns only on error.
pub fn restart(exe: &Path) -> std::io::Error {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        std::process::Command::new(exe).args(args).exec()
    }
    #[cfg(not(unix))]
    {
        match std::process::Command::new(exe).args(args).spawn() {
            Ok(_) => std::process::exit(0),
            Err(e) => e,
        }
    }
}

/// Ask the room owner for their client build and, if it is a different
/// version, download it and replace ourselves.
pub fn update(node: Node, current: String, events: Events) {
    tokio::spawn(async move {
        let result = async {
            let progress = {
                let events = events.clone();
                move |got, total| {
                    let _ = events.send(TaskEvent::UpdateProgress { got, total });
                }
            };
            let Some((version, data)) = node.fetch_update(&asset_name(), &current, progress).await? else {
                return Ok(None);
            };
            if !looks_like_executable(&data) {
                return Err("the download is not a program for this system — try again".to_string());
            }
            let exe = tokio::task::spawn_blocking(move || replace_self(&data))
                .await
                .map_err(spawn_err)?
                .map_err(|e| format!("cannot replace the program file: {e}"))?;
            Ok(Some((exe, version)))
        }
        .await;
        let _ = events.send(TaskEvent::UpdateDone(result));
    });
}

// ---------------------------------------------------------------- notifications

/// A desktop notification. On Linux through `notify-send`, if present;
/// otherwise quietly nothing (the terminal bell remains).
pub fn desktop_notify(title: &str, body: &str) {
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .args(["--app-name=tovuer", "--expire-time=5000", "--", title, body])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|mut child| {
                // no zombies: wait for it on the side
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (title, body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(w: u32, h: u32) -> Picture {
        let mut rgb = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = if (x / 8 + y / 8) % 2 == 0 { 255 } else { 0 };
                rgb.extend_from_slice(&[v, 0, 255 - v]);
            }
        }
        Picture { w, h, rgb, full: (w, h) }
    }

    #[test]
    fn cleanup_only_removes_a_committed_updater_backup() {
        let dir = std::env::temp_dir().join(format!("tovuer-update-clean-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let user_backup = dir.join("client.old");
        std::fs::write(&user_backup, b"user backup").unwrap();
        let backup = dir.join("client.tovuer-update");
        std::fs::create_dir(&backup).unwrap();
        let old = backup.join("previous.exe");
        std::fs::write(&old, b"original").unwrap();
        clean_update_backup(&backup);
        assert!(old.exists());
        std::fs::write(backup.join("committed"), UPDATE_COMMITTED).unwrap();
        clean_update_backup(&backup);
        assert!(!backup.exists());
        assert_eq!(std::fs::read(user_backup).unwrap(), b"user backup");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fit_keeps_aspect_and_even_height() {
        let wide = checker(400, 100).fit(40, 12);
        assert_eq!((wide.w, wide.h), (40, 10));
        let tall = checker(100, 400).fit(40, 12);
        assert_eq!((tall.w, tall.h), (6, 24));
        let odd = checker(30, 21).fit(40, 12);
        assert_eq!(odd.h % 2, 0);
        assert_eq!(odd.px.len(), odd.w as usize * odd.h as usize);
        // small pictures are not stretched
        let tiny = checker(4, 4).fit(40, 12);
        assert_eq!((tiny.w, tiny.h), (4, 4));
        assert_eq!(tiny.rows(), 2);
    }

    #[test]
    fn fit_averages_blocks() {
        // left half red, right half blue → two pixels of those colors
        let mut rgb = Vec::new();
        for _ in 0..10 {
            for x in 0..20 {
                rgb.extend_from_slice(if x < 10 { &[200, 0, 0] } else { &[0, 0, 200] });
            }
        }
        let t = Picture { w: 20, h: 10, rgb, full: (20, 10) }.fit(2, 1);
        assert_eq!((t.w, t.h), (2, 2));
        assert_eq!(t.at(0, 0), [200, 0, 0]);
        assert_eq!(t.at(1, 1), [0, 0, 200]);
    }

    #[test]
    fn decode_png_and_blend_alpha() {
        let mut img = image::RgbaImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.put_pixel(1, 0, image::Rgba([255, 255, 255, 0]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(img).write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        let pic = Picture::decode(&bytes).unwrap();
        assert_eq!((pic.w, pic.h), (2, 1));
        assert_eq!(&pic.rgb[0..3], &[255, 0, 0]);
        assert_eq!(&pic.rgb[3..6], &[24, 24, 28], "a transparent pixel takes the backdrop color");
        assert!(Picture::decode(b"this is not an image").is_err());
        assert!(Picture::decode(&[]).is_err());
    }

    #[test]
    fn huge_pictures_are_downscaled_on_load() {
        let img = image::RgbImage::from_pixel(3000, 1500, image::Rgb([10, 20, 30]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        let pic = Picture::decode(&bytes).unwrap();
        assert!(pic.w <= PICTURE_MAX_SIDE && pic.h <= PICTURE_MAX_SIDE);
        assert_eq!(pic.w, 2 * pic.h);
        assert_eq!(pic.full, (3000, 1500), "the original size is remembered");
    }

    fn png(img: image::RgbImage) -> Vec<u8> {
        let mut bytes = Vec::new();
        DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        bytes
    }

    #[test]
    fn sharp_pictures_keep_every_pixel_that_fits() {
        // a 10×20-pixel font: a 40×12-cell viewer holds 400×240 pixels
        let mut graphics = Graphics::halfblocks(); // its font: 10×20 px
        for kind in [ProtocolType::Sixel, ProtocolType::Kitty, ProtocolType::Iterm2] {
            graphics.set_protocol_type(kind);
            // smaller than the viewer: drawn at its own size, never enlarged
            let small = decode_image(&png(image::RgbImage::from_pixel(95, 41, image::Rgb([200, 10, 10])))).unwrap();
            let sharp = Sharp::encode(&small, &graphics, 40, 12, [0, 0, 0]).unwrap();
            assert_eq!(sharp.size(), (10, 3), "{kind:?}: 95×41 px is 10×3 cells");
            assert_eq!(sharp.fragile(), kind != ProtocolType::Kitty);
            // bigger: scaled down to fill the viewer, proportions kept
            let big = decode_image(&png(image::RgbImage::from_pixel(4000, 1000, image::Rgb([10, 200, 10])))).unwrap();
            let sharp = Sharp::encode(&big, &graphics, 40, 12, [0, 0, 0]).unwrap();
            assert_eq!(sharp.size(), (40, 5), "{kind:?}: 400×100 px");
        }
    }

    #[test]
    fn sharp_pictures_draw_into_the_buffer() {
        use ratatui::widgets::Widget;
        let mut graphics = Graphics::halfblocks(); // its font: 10×20 px
        graphics.set_protocol_type(ProtocolType::Sixel);
        let img = decode_image(&png(image::RgbImage::from_pixel(100, 60, image::Rgb([1, 2, 3])))).unwrap();
        let sharp = Sharp::encode(&img, &graphics, 40, 12, [0, 0, 0]).unwrap();
        let area = ratatui::layout::Rect::new(0, 0, 40, 12);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui_image::Image::new(&sharp.proto).render(area, &mut buf);
        assert!(buf[(0, 0)].symbol().contains("\x1bP"), "the first cell carries the sixel data");
    }

    #[test]
    fn photos_are_turned_upright() {
        // a JPEG 4 px wide and 2 px tall, marked "rotate 90°" (EXIF orientation 6)
        let img = image::RgbImage::from_pixel(4, 2, image::Rgb([50, 100, 150]));
        let mut jpeg = Vec::new();
        DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();
        let exif: &[u8] = &[
            b'E', b'x', b'i', b'f', 0, 0, // header
            b'M', b'M', 0, 42, 0, 0, 0, 8, // TIFF, big endian, first directory at 8
            0, 1, // one entry
            0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, // Orientation = 6
            0, 0, 0, 0, // no next directory
        ];
        let mut marked = jpeg[..2].to_vec(); // SOI
        marked.extend_from_slice(&[0xFF, 0xE1]);
        marked.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        marked.extend_from_slice(exif);
        marked.extend_from_slice(&jpeg[2..]);
        let upright = decode_image(&marked).unwrap();
        assert_eq!((upright.width(), upright.height()), (2, 4));
        let pic = Picture::decode(&marked).unwrap();
        assert_eq!((pic.w, pic.h, pic.full), (2, 4, (2, 4)));
    }

    #[test]
    fn names_for_the_system_app_are_portable() {
        assert_eq!(portable_name("cat.png"), "cat.png");
        assert_eq!(portable_name("a:b*c?.jpg"), "a_b_c_.jpg");
        assert_eq!(portable_name("..."), "file");
    }

    #[test]
    fn update_payload_must_look_like_a_program() {
        let html = b"<html><body>502 Bad Gateway</body></html>".repeat(20_000);
        assert!(!looks_like_executable(&html));
        assert!(!looks_like_executable(b"\x7fELF"), "too small");
        let mut real = if cfg!(windows) { b"MZ".to_vec() } else { b"\x7fELF".to_vec() };
        real.resize(512 * 1024, 0);
        assert!(looks_like_executable(&real));
    }

    #[test]
    fn asset_name_matches_release_layout() {
        let name = asset_name();
        assert!(name.starts_with("tovuer-"));
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(name, "tovuer-linux-amd64");
        }
    }
}
