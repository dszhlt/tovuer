//! Moving previews: animated GIFs and WebPs, and the first seconds of
//! videos, play in the feed silently and in a loop.
//!
//! A clip is decoded once into frames at a modest size; the feed's blocks
//! keep them shrunk to the thumbnail, the terminal's own graphics get each
//! frame encoded once and then only switch between them.

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use image::{AnimationDecoder, ImageDecoder, RgbImage};

use crate::transfer::{BACKDROP, Graphics, Picture, Sharp, VIDEO_FORMATS, blend, ffmpeg_input, run_for};

/// A video plays its first this many seconds, over and over.
pub const CLIP_SECONDS: u32 = 12;
/// …at this many frames a second.
pub const CLIP_FPS: u32 = 10;
/// Frames are kept no bigger than this: plenty for a feed thumbnail.
const CLIP_SIDE: u32 = 360;
const MAX_FRAMES: usize = 150;
/// All frames of a clip together, in pixels — a guard against "bombs".
const MAX_PIXELS: u64 = 12 << 20;
/// Decoding a picture's frames stops after this long.
const DECODE_BUDGET: Duration = Duration::from_secs(8);
/// A GIF frame asking for less than this means "as fast as you can";
/// browsers show it at 100 ms, and so does this.
const MIN_DELAY: u32 = 20;
const DEFAULT_DELAY: u32 = 100;

/// Frames of a picture or a video and how long each stays, in ms.
pub struct Clip {
    pub frames: Vec<(RgbImage, u32)>,
}

fn total(delays: impl Iterator<Item = u32>) -> u32 {
    delays.sum::<u32>().max(1)
}

/// Which frame is on `ms` into a loop of `delays`.
fn index_at(delays: &[u32], total: u32, ms: u64) -> usize {
    let mut t = (ms % total as u64) as u32;
    for (i, d) in delays.iter().enumerate() {
        if t < *d {
            return i;
        }
        t -= d;
    }
    delays.len().saturating_sub(1)
}

/// The frames of a moving thumbnail in blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anim {
    pub frames: Vec<Vec<[u8; 3]>>,
    pub delays: Vec<u32>,
    total: u32,
}

impl Anim {
    pub fn index_at(&self, ms: u64) -> usize {
        index_at(&self.delays, self.total, ms)
    }
}

/// The frames of a moving thumbnail in the terminal's own graphics.
pub struct SharpClip {
    pub frames: Vec<Arc<Sharp>>,
    pub delays: Vec<u32>,
    total: u32,
}

/// A feed thumbnail in the terminal's own graphics: still or moving.
pub enum Pixels {
    Still(Arc<Sharp>),
    Moving(Arc<SharpClip>),
}

impl std::fmt::Debug for Pixels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Pixels::Still(s) => write!(f, "Still({s:?})"),
            Pixels::Moving(c) => write!(f, "Moving({} frames)", c.frames.len()),
        }
    }
}

impl Pixels {
    /// How many cells it covers.
    pub fn size(&self) -> (u16, u16) {
        self.at(0).size()
    }

    /// The picture to show `ms` into the animation.
    pub fn at(&self, ms: u64) -> &Arc<Sharp> {
        match self {
            Pixels::Still(sharp) => sharp,
            Pixels::Moving(clip) => &clip.frames[index_at(&clip.delays, clip.total, ms)],
        }
    }

    pub fn moving(&self) -> bool {
        matches!(self, Pixels::Moving(_))
    }
}

/// A picture that may be animated, by its name: GIF and WebP.
pub fn may_move(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".gif") || lower.ends_with(".webp")
}

/// The moving frames of a file; `None` — it does not move (a still GIF,
/// a WebP without animation, anything else).
pub fn load_clip(path: &Path, video: bool) -> Result<Option<Clip>, String> {
    let frames = if video {
        video_frames(path)?
    } else {
        let bytes = std::fs::read(path).map_err(|e| format!("cannot read: {e}"))?;
        picture_frames(&bytes)?
    };
    Ok((frames.len() > 1).then_some(Clip { frames }))
}

/// Shrink a frame to keep (never enlarged), on the dark backdrop.
fn keep(frame: image::RgbaImage) -> RgbImage {
    let backdrop = BACKDROP.map(|c| c as u8);
    let (w, h) = frame.dimensions();
    let frame = if w > CLIP_SIDE || h > CLIP_SIDE { image::imageops::thumbnail(&frame, CLIP_SIDE, CLIP_SIDE) } else { frame };
    RgbImage::from_fn(frame.width(), frame.height(), |x, y| image::Rgb(blend(*frame.get_pixel(x, y), backdrop)))
}

/// The frames of an animated GIF or WebP.
fn picture_frames(bytes: &[u8]) -> Result<Vec<(RgbImage, u32)>, String> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(128 << 20);
    let frames = if bytes.starts_with(b"GIF8") {
        let mut decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
        decoder.set_limits(limits).map_err(|e| e.to_string())?;
        decoder.into_frames()
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
        if !decoder.has_animation() {
            return Ok(Vec::new());
        }
        decoder.set_limits(limits).map_err(|e| e.to_string())?;
        decoder.into_frames()
    } else {
        return Ok(Vec::new());
    };
    let started = Instant::now();
    let mut out = Vec::new();
    let mut pixels = 0u64;
    for frame in frames {
        let Ok(frame) = frame else { break }; // a damaged tail: play what came before it
        let (n, d) = frame.delay().numer_denom_ms();
        let ms = n.checked_div(d).unwrap_or(0);
        let kept = keep(frame.into_buffer());
        pixels += kept.width() as u64 * kept.height() as u64;
        out.push((kept, if ms < MIN_DELAY { DEFAULT_DELAY } else { ms.min(10_000) }));
        if out.len() >= MAX_FRAMES || pixels >= MAX_PIXELS || started.elapsed() > DECODE_BUDGET {
            break;
        }
    }
    Ok(out)
}

/// The first seconds of a video, through ffmpeg, as PPM frames.
fn video_frames(path: &Path) -> Result<Vec<(RgbImage, u32)>, String> {
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-nostdin", "-v", "error", "-protocol_whitelist", "file", "-format_whitelist", VIDEO_FORMATS])
        .arg("-i")
        .arg(ffmpeg_input(path))
        .args(["-t", &CLIP_SECONDS.to_string(), "-an", "-sn", "-dn"])
        // shrunk to fit, never enlarged
        .args(["-vf", &format!("fps={CLIP_FPS},scale=w='min({CLIP_SIDE},iw)':h='min({CLIP_SIDE},ih)':force_original_aspect_ratio=decrease")])
        .args(["-frames:v", &MAX_FRAMES.to_string(), "-f", "image2pipe", "-c:v", "ppm", "pipe:1"]);
    let limit = MAX_FRAMES as u64 * CLIP_SIDE as u64 * CLIP_SIDE as u64 * 3 + (1 << 20);
    let out = run_for(cmd, Duration::from_secs(30), limit)?;
    let delay = 1000 / CLIP_FPS;
    Ok(parse_ppms(&out).into_iter().map(|f| (f, delay)).collect())
}

/// Binary PPM pictures one after another: `P6 <w> <h> 255` and the pixels.
fn parse_ppms(mut data: &[u8]) -> Vec<RgbImage> {
    let mut frames = Vec::new();
    while data.starts_with(b"P6") {
        // three numbers after the magic, each ended by one whitespace byte
        let mut fields = [0u32; 3];
        let mut at = 2;
        for field in &mut fields {
            while data.get(at).is_some_and(u8::is_ascii_whitespace) {
                at += 1;
            }
            let start = at;
            while data.get(at).is_some_and(u8::is_ascii_digit) {
                at += 1;
            }
            *field = std::str::from_utf8(&data[start..at]).ok().and_then(|s| s.parse().ok()).unwrap_or(0);
        }
        at += 1;
        let [w, h, max] = fields;
        let len = w as usize * h as usize * 3;
        if w == 0 || h == 0 || w > 4096 || h > 4096 || max != 255 || data.len() < at + len {
            break;
        }
        if let Some(img) = RgbImage::from_raw(w, h, data[at..at + len].to_vec()) {
            frames.push(img);
        }
        data = &data[at + len..];
    }
    frames
}

/// The clip in blocks, every frame exactly `w`×`h` like the still thumbnail.
pub fn anim_for(clip: &Clip, w: u16, h: u16) -> Anim {
    let frames = clip
        .frames
        .iter()
        .map(|(img, _)| {
            let pic = Picture { w: img.width(), h: img.height(), rgb: img.as_raw().clone(), full: img.dimensions(), duration: None };
            pic.scale_to(w as u32, h as u32)
        })
        .collect();
    let delays: Vec<u32> = clip.frames.iter().map(|(_, d)| *d).collect();
    Anim { frames, total: total(delays.iter().copied()), delays }
}

/// The clip for the terminal's own graphics, every frame covering `cells`.
pub fn sharp_clip(clip: &Clip, graphics: &Graphics, cells: (u16, u16)) -> Result<SharpClip, String> {
    let mut frames = Vec::with_capacity(clip.frames.len());
    for (img, _) in &clip.frames {
        let img = image::DynamicImage::ImageRgb8(img.clone());
        frames.push(Arc::new(Sharp::encode_filled(&img, graphics, cells.0, cells.1)?));
    }
    let delays: Vec<u32> = clip.frames.iter().map(|(_, d)| *d).collect();
    Ok(SharpClip { frames, total: total(delays.iter().copied()), delays })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An animated GIF: `n` frames of solid colors, 50 ms each.
    pub(crate) fn gif(n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut out);
            enc.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
            for i in 0..n {
                let color = [(i * 80 % 256) as u8, 40, 200 - (i * 40 % 200) as u8, 255];
                let img = image::RgbaImage::from_pixel(40, 30, image::Rgba(color));
                let frame = image::Frame::from_parts(img, 0, 0, image::Delay::from_numer_denom_ms(50, 1));
                enc.encode_frame(frame).unwrap();
            }
        }
        out
    }

    #[test]
    fn a_gif_comes_apart_into_frames() {
        let frames = picture_frames(&gif(3)).unwrap();
        assert_eq!(frames.len(), 3);
        assert!(frames.iter().all(|(img, d)| img.dimensions() == (40, 30) && *d == 50));
        assert_ne!(frames[0].0.get_pixel(5, 5), frames[1].0.get_pixel(5, 5));
        assert_eq!(picture_frames(&gif(1)).unwrap().len(), 1, "a still GIF is one frame");
        assert!(picture_frames(b"\x89PNG not a gif").unwrap().is_empty());
        assert!(picture_frames(b"GIF89a garbage").is_err() || picture_frames(b"GIF89a garbage").unwrap().is_empty());
    }

    #[test]
    fn frames_follow_the_clock_and_loop() {
        let delays = [100, 50, 200];
        let total = total(delays.iter().copied());
        let at = |ms| index_at(&delays, total, ms);
        assert_eq!((at(0), at(99), at(100), at(149), at(150), at(349)), (0, 0, 1, 1, 2, 2));
        assert_eq!(at(350), 0, "round again");
        assert_eq!(index_at(&[], 1, 5), 0);
    }

    #[test]
    fn blocks_keep_the_thumbnails_size() {
        let clip = Clip { frames: picture_frames(&gif(4)).unwrap() };
        let anim = anim_for(&clip, 12, 8);
        assert_eq!(anim.frames.len(), 4);
        assert!(anim.frames.iter().all(|f| f.len() == 12 * 8));
        assert_eq!(anim.index_at(60), 1);
    }

    #[test]
    fn ppm_streams_split_into_frames() {
        let mut data = Vec::new();
        for v in [10u8, 200] {
            data.extend_from_slice(b"P6\n2 1\n255\n");
            data.extend_from_slice(&[v; 6]);
        }
        let frames = parse_ppms(&data);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].get_pixel(1, 0)[0], 200);
        assert_eq!(parse_ppms(b"P6\n99999 1\n255\n").len(), 0, "a lying header stops it");
    }

    /// ffmpeg's test video through the real path (skipped without ffmpeg).
    #[test]
    fn a_video_gives_its_first_seconds() {
        let dir = std::env::temp_dir().join(format!("tovuer-motion-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mp4");
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25:duration=2", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("no ffmpeg — skipped");
            return;
        }
        let clip = load_clip(&path, true).unwrap().expect("a video moves");
        assert!((18..=21).contains(&clip.frames.len()), "two seconds at {CLIP_FPS} fps: {}", clip.frames.len());
        assert_eq!(clip.frames[0].0.dimensions(), (320, 240));
        assert_eq!(clip.frames[0].1, 100);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
