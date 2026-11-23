//! Audio devices: listing, picking by name and opening streams through cpal
//! (PulseAudio/PipeWire or ALSA on Linux, WASAPI on Windows).
//!
//! A device speaks its own format, rate and channel count — here everything
//! is converted to the internal 48 kHz mono f32 and back.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, FromSample, Sample, SampleFormat, SizedSample, StreamConfig, SupportedBufferSize};

use crate::capture::{CaptureProcessor, Packet};
use crate::mixer::Mixer;
use crate::resample::Resampler;
use crate::{FRAME, SAMPLE_RATE};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioError(pub String);

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AudioError {}

impl From<cpal::Error> for AudioError {
    fn from(e: cpal::Error) -> Self {
        Self(e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub is_default: bool,
}

/// What actually opened.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamInfo {
    /// The audio system: PulseAudio, ALSA, WASAPI…
    pub host: String,
    /// `None` — the microphone could not be opened (listen-only mode).
    pub mic: Option<String>,
    pub spk: Option<String>,
    pub mic_rate: u32,
    pub spk_rate: u32,
    /// What went wrong without stopping the start.
    pub warnings: Vec<String>,
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .or_else(|_| device.id().map(|id| id.to_string()))
        .unwrap_or_else(|_| "?".to_string())
}

/// "Monitor of …" records what plays in the speakers — it is not a microphone.
fn is_monitor(name: &str) -> bool {
    name.starts_with("Monitor of ")
}

/// The system's microphones and speakers.
pub fn list_devices() -> Result<(Vec<DeviceInfo>, Vec<DeviceInfo>), AudioError> {
    let host = cpal::default_host();
    let default_in = host.default_input_device();
    let default_out = host.default_output_device();
    let mut inputs: Vec<DeviceInfo> = host
        .input_devices()?
        .map(|d| DeviceInfo { name: device_name(&d), is_default: default_in.as_ref() == Some(&d) })
        .collect();
    inputs.sort_by_key(|d| is_monitor(&d.name));
    let outputs = host
        .output_devices()?
        .map(|d| DeviceInfo { name: device_name(&d), is_default: default_out.as_ref() == Some(&d) })
        .collect();
    Ok((inputs, outputs))
}

/// Find a device by part of its name (any case); empty — the system default.
/// The second value is `true` if a specific one was asked for, not found, and the default was used.
fn pick_device(host: &cpal::Host, want: &str, input: bool) -> Result<(cpal::Device, bool), AudioError> {
    let default = || {
        let device = if input { host.default_input_device() } else { host.default_output_device() };
        device.ok_or_else(|| AudioError(if input { "no microphone".into() } else { "no output device".into() }))
    };
    let want = want.trim().to_lowercase();
    if want.is_empty() {
        return Ok((default()?, false));
    }
    let mut devices: Vec<cpal::Device> =
        if input { host.input_devices()?.collect() } else { host.output_devices()?.collect() };
    // an exact match beats a substring: "Built-in Audio" must not catch "Monitor of Built-in Audio"
    devices.sort_by_key(|d| {
        let name = device_name(d).to_lowercase();
        (name != want, is_monitor(&device_name(d)), name.len())
    });
    match devices.into_iter().find(|d| device_name(d).to_lowercase().contains(&want)) {
        Some(device) => Ok((device, false)),
        None => Ok((default()?, true)),
    }
}

/// The device's native configuration, moved to 48 kHz where possible and with
/// a small buffer (10 ms) for latency.
fn choose_config(device: &cpal::Device, input: bool) -> Result<(StreamConfig, SampleFormat), AudioError> {
    let default = if input { device.default_input_config()? } else { device.default_output_config()? };
    let format = default.sample_format();
    let mut config = default.config();
    if config.sample_rate != SAMPLE_RATE {
        let ranges: Vec<cpal::SupportedStreamConfigRange> = if input {
            device.supported_input_configs().map(|r| r.collect()).unwrap_or_default()
        } else {
            device.supported_output_configs().map(|r| r.collect()).unwrap_or_default()
        };
        let native_48k = ranges.iter().any(|r| {
            r.channels() == config.channels && r.sample_format() == format && r.contains_rate(SAMPLE_RATE)
        });
        if native_48k {
            config.sample_rate = SAMPLE_RATE;
        }
    }
    config.buffer_size = match default.buffer_size() {
        SupportedBufferSize::Range { min, max } => {
            BufferSize::Fixed((config.sample_rate / 100).clamp(*min, (*max).max(*min)))
        }
        SupportedBufferSize::Unknown => BufferSize::Default,
    };
    Ok((config, format))
}

/// Dispatch on the device's sample format: the body is instantiated with the right type `$t`.
macro_rules! with_sample_type {
    ($format:expr, $t:ident => $body:expr) => {
        match $format {
            SampleFormat::F32 => { type $t = f32; $body }
            SampleFormat::I16 => { type $t = i16; $body }
            SampleFormat::I32 => { type $t = i32; $body }
            SampleFormat::U16 => { type $t = u16; $body }
            SampleFormat::I8 => { type $t = i8; $body }
            SampleFormat::U8 => { type $t = u8; $body }
            SampleFormat::I24 => { type $t = cpal::I24; $body }
            SampleFormat::U24 => { type $t = cpal::U24; $body }
            SampleFormat::U32 => { type $t = u32; $body }
            SampleFormat::I64 => { type $t = i64; $body }
            SampleFormat::U64 => { type $t = u64; $body }
            SampleFormat::F64 => { type $t = f64; $body }
            other => Err(AudioError(format!("sample format {other} is not supported"))),
        }
    };
}

type ErrorSlot = Arc<Mutex<Option<String>>>;

fn report(slot: &ErrorSlot, what: &str) -> impl FnMut(cpal::Error) + Send + 'static + use<> {
    let slot = slot.clone();
    let what = what.to_string();
    move |err| {
        // buffer overruns/underruns are part of the job, not crashes
        if matches!(err.kind(), cpal::ErrorKind::Xrun | cpal::ErrorKind::RealtimeDenied) {
            return;
        }
        if let Ok(mut slot) = slot.lock() {
            *slot = Some(format!("{what}: {err}"));
        }
    }
}

fn input_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut sink: impl FnMut(&[f32]) + Send + 'static,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = config.channels.max(1) as usize;
    let mut mono: Vec<f32> = Vec::new();
    device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            mono.clear();
            mono.extend(data.chunks_exact(channels).map(|frame| {
                frame.iter().map(|s| f32::from_sample(*s)).sum::<f32>() / channels as f32
            }));
            sink(&mono);
        },
        on_error,
        None,
    )
}

fn output_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut render: impl FnMut(&mut [f32]) + Send + 'static,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels.max(1) as usize;
    let mut mono: Vec<f32> = Vec::new();
    device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            mono.clear();
            mono.resize(data.len() / channels, 0.0);
            render(&mut mono);
            for (frame, sample) in data.chunks_exact_mut(channels).zip(mono.iter()) {
                frame.fill(T::from_sample(*sample));
            }
        },
        on_error,
        None,
    )
}

/// Open the microphone: its samples go to the processor, finished packets to `on_packet`.
fn open_input(
    device: &cpal::Device,
    make_processor: &dyn Fn() -> Result<CaptureProcessor, AudioError>,
    on_packet: Arc<dyn Fn(Packet) + Send + Sync>,
    errors: &ErrorSlot,
) -> Result<(cpal::Stream, u32), AudioError> {
    let (config, format) = choose_config(device, true)?;
    let build = |config: StreamConfig| -> Result<cpal::Stream, AudioError> {
        let mut processor = make_processor()?;
        let mut resampler = Resampler::new(config.sample_rate, SAMPLE_RATE);
        let mut at_48k: Vec<f32> = Vec::new();
        let on_packet = on_packet.clone();
        let sink = move |mono: &[f32]| {
            at_48k.clear();
            resampler.process(mono, &mut at_48k);
            processor.feed(&at_48k, &mut |packet| on_packet(packet));
        };
        with_sample_type!(format, T => input_stream::<T>(device, config, sink, report(errors, "microphone")).map_err(AudioError::from))
    };
    let stream = match build(config) {
        Ok(stream) => stream,
        // not every device accepts a small buffer — try its native one
        Err(_) if config.buffer_size != BufferSize::Default => {
            build(StreamConfig { buffer_size: BufferSize::Default, ..config })?
        }
        Err(e) => return Err(e),
    };
    stream.play()?;
    Ok((stream, config.sample_rate))
}

/// Open the output: frames come from `mixer`.
fn open_output(
    device: &cpal::Device,
    mixer: Arc<Mutex<Mixer>>,
    errors: &ErrorSlot,
) -> Result<(cpal::Stream, u32), AudioError> {
    let (config, format) = choose_config(device, false)?;
    let build = |config: StreamConfig| -> Result<cpal::Stream, AudioError> {
        let mut resampler = Resampler::new(SAMPLE_RATE, config.sample_rate);
        let mut ready: VecDeque<f32> = VecDeque::new();
        let mut frame = [0.0f32; FRAME];
        let mut scratch: Vec<f32> = Vec::new();
        let mixer = mixer.clone();
        let render = move |out: &mut [f32]| {
            while ready.len() < out.len() {
                mixer.lock().unwrap_or_else(|e| e.into_inner()).mix(&mut frame);
                scratch.clear();
                resampler.process(&frame, &mut scratch);
                ready.extend(scratch.iter().copied());
            }
            for sample in out.iter_mut() {
                *sample = ready.pop_front().unwrap_or(0.0);
            }
        };
        with_sample_type!(format, T => output_stream::<T>(device, config, render, report(errors, "audio output")).map_err(AudioError::from))
    };
    let stream = match build(config) {
        Ok(stream) => stream,
        Err(_) if config.buffer_size != BufferSize::Default => {
            build(StreamConfig { buffer_size: BufferSize::Default, ..config })?
        }
        Err(e) => return Err(e),
    };
    stream.play()?;
    Ok((stream, config.sample_rate))
}

/// The open streams. While the value lives, audio flows; drop closes the devices.
pub struct Streams {
    _input: Option<cpal::Stream>,
    _output: Option<cpal::Stream>,
    pub info: StreamInfo,
}

/// Open the microphone and output. If at least one of them comes up, that is
/// success with a warning (you can listen without a microphone and vice versa).
pub fn open(
    mic: &str,
    spk: &str,
    make_processor: &dyn Fn() -> Result<CaptureProcessor, AudioError>,
    on_packet: Arc<dyn Fn(Packet) + Send + Sync>,
    mixer: Arc<Mutex<Mixer>>,
    errors: &ErrorSlot,
) -> Result<Streams, AudioError> {
    let host = cpal::default_host();
    let mut info = StreamInfo { host: host.id().name().to_string(), ..StreamInfo::default() };

    let input = pick_device(&host, mic, true).and_then(|(device, fell_back)| {
        if fell_back {
            info.warnings.push(format!("microphone “{mic}” not found — using the system default"));
        }
        let name = device_name(&device);
        let (stream, rate) = open_input(&device, make_processor, on_packet.clone(), errors)?;
        Ok((stream, name, rate))
    });
    let output = pick_device(&host, spk, false).and_then(|(device, fell_back)| {
        if fell_back {
            info.warnings.push(format!("speakers “{spk}” not found — using the system default"));
        }
        let name = device_name(&device);
        let (stream, rate) = open_output(&device, mixer.clone(), errors)?;
        Ok((stream, name, rate))
    });

    match (input, output) {
        (Err(mic_err), Err(spk_err)) => Err(AudioError(format!("microphone: {mic_err}; output: {spk_err}"))),
        (input, output) => {
            let mut streams = Streams { _input: None, _output: None, info };
            match input {
                Ok((stream, name, rate)) => {
                    streams._input = Some(stream);
                    streams.info.mic = Some(name);
                    streams.info.mic_rate = rate;
                }
                Err(e) => streams.info.warnings.push(format!("the microphone did not open ({e}) — nobody will hear you")),
            }
            match output {
                Ok((stream, name, rate)) => {
                    streams._output = Some(stream);
                    streams.info.spk = Some(name);
                    streams.info.spk_rate = rate;
                }
                Err(e) => streams.info.warnings.push(format!("audio output did not open ({e}) — you will hear no one")),
            }
            Ok(streams)
        }
    }
}
