//! H.264 encoding with GStreamer: raw frames in through `appsrc`, Annex B
//! access units out through `appsink` onto a broadcast channel.

use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use tokio::sync::broadcast;

#[derive(Clone, Copy, Debug, PartialEq, clap::ValueEnum)]
pub enum Codec {
    /// NVENC if available, else x264
    Auto,
    /// CPU (libx264)
    X264,
    /// NVIDIA NVENC (falls back to x264 if it fails)
    Nvenc,
}

impl Codec {
    /// Pick a concrete encoder. The nvcodec plugin only registers `nvh264enc`
    /// when a usable NVIDIA GPU is present.
    pub fn resolve(self) -> Codec {
        match self {
            Codec::Auto if gst::ElementFactory::find("nvh264enc").is_some() => Codec::Nvenc,
            Codec::Auto => Codec::X264,
            other => other,
        }
    }
}

#[derive(Debug)]
pub struct EncodedFrame {
    pub keyframe: bool,
    pub pts_us: u64,
    pub data: Vec<u8>,
}

/// The GStreamer elements of one running pipeline. Replaced as a whole when the
/// encoder falls back (see [`Encoder::fall_back`]).
struct Pipe {
    pipeline: gst::Pipeline,
    appsrc: gst_app::AppSrc,
    appsink: gst_app::AppSink,
}

pub struct Encoder {
    /// Lock order: `pipe` before `caps`.
    pipe: Mutex<Pipe>,
    codec: Mutex<Codec>,
    bitrate_kbps: u32,
    frames: broadcast::Sender<Arc<EncodedFrame>>,
    /// Raw format of the frames, kept to configure a replacement pipeline.
    caps: Mutex<Option<gst::Caps>>,
    /// Bumped whenever the pipeline is replaced or stopped, so a bus thread
    /// that belongs to an old pipeline can tell and exit.
    generation: AtomicU32,
    /// Last raw frame, re-encoded as a keyframe when a client needs one while
    /// the screen is static (Mutter only sends frames on damage).
    last: Mutex<Option<gst::Buffer>>,
    last_keyframe_request: Mutex<Option<Instant>>,
    /// Buffer timestamps are time since `epoch`.
    epoch: Instant,
    pushed: Arc<AtomicU32>,
}

/// Encode latency and throughput, logged every few seconds while frames flow.
struct Stats {
    since: Instant,
    pushed: Arc<AtomicU32>,
    frames: u32,
    bytes: usize,
    total: Duration,
    max: Duration,
}

impl Stats {
    const INTERVAL: Duration = Duration::from_secs(5);

    fn new(pushed: Arc<AtomicU32>) -> Self {
        Self { since: Instant::now(), pushed, frames: 0, bytes: 0, total: Duration::ZERO, max: Duration::ZERO }
    }

    fn add(&mut self, latency: Duration, bytes: usize) {
        self.frames += 1;
        self.bytes += bytes;
        self.total += latency;
        self.max = self.max.max(latency);
        let elapsed = self.since.elapsed();
        if elapsed >= Self::INTERVAL {
            let secs = elapsed.as_secs_f64();
            eprintln!(
                "encoder: {:.0} fps in, {:.0} fps out, {:.1} Mbit/s, encode latency avg {:.1} ms, max {:.1} ms",
                self.pushed.swap(0, Relaxed) as f64 / secs,
                self.frames as f64 / secs,
                self.bytes as f64 * 8.0 / secs / 1e6,
                self.total.as_secs_f64() * 1000.0 / self.frames as f64,
                self.max.as_secs_f64() * 1000.0
            );
            *self = Self::new(self.pushed.clone());
        }
    }
}

/// Keyframes are ~10× the size of other frames; don't let clients ask for them back to back.
const MIN_KEYFRAME_INTERVAL: Duration = Duration::from_millis(250);

/// How much video the rate-control buffer may hold. A small buffer stops an
/// animation from bursting far above the target bitrate (which queues up on a
/// slow link); it also limits keyframes, so it can't be much smaller.
const VBV_MS: u32 = 150;

/// The encoder part of the pipeline: constant bitrate with a small VBV buffer.
fn encoder_description(codec: Codec, bitrate_kbps: u32) -> String {
    let vbv_kbit = bitrate_kbps * VBV_MS / 1000;
    match codec {
        Codec::Nvenc => format!(
            // NVENC takes BGRx/RGBx directly and converts on the GPU.
            "nvh264enc preset=p1 tune=ultra-low-latency zerolatency=true bframes=0 \
             rc-mode=cbr bitrate={bitrate_kbps} vbv-buffer-size={vbv_kbit} gop-size=600"
        ),
        _ => format!(
            "videoconvert n-threads=4 ! video/x-raw,format=I420 ! x264enc tune=zerolatency \
             speed-preset=ultrafast pass=cbr bitrate={bitrate_kbps} vbv-buf-capacity={VBV_MS} \
             key-int-max=600 bframes=0 sliced-threads=true"
        ),
    }
}

/// The encoder to switch to when `codec` fails, or `None` when there is nothing
/// simpler left to try. x264 runs on the CPU everywhere, so it is the last resort.
fn fallback_codec(codec: Codec) -> Option<Codec> {
    match codec {
        Codec::Nvenc => Some(Codec::X264),
        Codec::Auto | Codec::X264 => None,
    }
}

/// Test hook: `TABSCREEN_TEST_NVENC_FAIL=start` makes the NVENC pipeline fail to
/// build; `=runtime` makes it report an error 5 s after it starts. Only NVENC is
/// affected, so the fallback path can be tried on a machine where NVENC works.
fn test_failure() -> Option<String> {
    std::env::var("TABSCREEN_TEST_NVENC_FAIL").ok()
}

/// Build and start a pipeline for `codec`. Encoded frames go to `frames`;
/// `pushed` counts raw frames for the stats line.
fn build_pipe(
    codec: Codec,
    bitrate_kbps: u32,
    frames: broadcast::Sender<Arc<EncodedFrame>>,
    pushed: Arc<AtomicU32>,
) -> Result<Pipe> {
    let mut encode = encoder_description(codec, bitrate_kbps);
    let test = if codec == Codec::Nvenc { test_failure() } else { None };
    if test.as_deref() == Some("start") {
        encode = encode.replacen("nvh264enc", "tabscreen-test-missing-element", 1);
    }
    let desc = format!(
        "appsrc name=src is-live=true format=time max-buffers=2 leaky-type=downstream \
         ! {encode} ! video/x-h264,profile=constrained-baseline \
         ! h264parse config-interval=-1 ! video/x-h264,stream-format=byte-stream,alignment=au \
         ! appsink name=sink sync=false max-buffers=4"
    );
    let pipeline = gst::parse::launch(&desc)
        .with_context(|| format!("building pipeline: {desc}"))?
        .downcast::<gst::Pipeline>()
        .unwrap();
    let appsrc = pipeline.by_name("src").unwrap().downcast::<gst_app::AppSrc>().unwrap();
    let appsink = pipeline.by_name("sink").unwrap().downcast::<gst_app::AppSink>().unwrap();

    // When each frame left appsrc's queue. Encoders rewrite timestamps,
    // but they're 1:1 here, so frames come out in the same order.
    let in_flight: Arc<Mutex<VecDeque<Instant>>> = Arc::default();
    let entered = in_flight.clone();
    appsrc.static_pad("src").unwrap().add_probe(
        gst::PadProbeType::BUFFER,
        move |_, _| {
            entered.lock().unwrap().push_back(Instant::now());
            gst::PadProbeReturn::Ok
        },
    );
    let mut stats = Stats::new(pushed);
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                static FIRST: std::sync::Once = std::sync::Once::new();
                FIRST.call_once(|| eprintln!("encoder: first encoded frame ({} bytes)", map.len()));
                if let Some(entered) = in_flight.lock().unwrap().pop_front() {
                    stats.add(entered.elapsed(), map.len());
                }
                let _ = frames.send(Arc::new(EncodedFrame {
                    keyframe: !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT),
                    pts_us: buffer.pts().map_or(0, |t| t.useconds()),
                    data: map.to_vec(),
                }));
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    pipeline.set_state(gst::State::Playing).context("starting encoder pipeline")?;
    if test.as_deref() == Some("runtime") {
        let bus_pipeline = pipeline.downgrade();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(5));
            if let Some(p) = bus_pipeline.upgrade() {
                let _ = p.post_message(gst::message::Error::new(gst::LibraryError::Failed, "test failure"));
            }
        });
    }
    Ok(Pipe { pipeline, appsrc, appsink })
}

impl Encoder {
    /// Start encoding with `codec` (`Auto` is resolved). If a hardware encoder
    /// cannot start, falls back to x264 and says so in the log.
    pub fn new(codec: Codec, bitrate_kbps: u32, frames: broadcast::Sender<Arc<EncodedFrame>>) -> Result<Arc<Self>> {
        let pushed = Arc::new(AtomicU32::new(0));
        let mut codec = codec.resolve();
        let pipe = match build_pipe(codec, bitrate_kbps, frames.clone(), pushed.clone()) {
            Ok(pipe) => pipe,
            Err(e) => {
                let Some(next) = fallback_codec(codec) else { return Err(e) };
                eprintln!("encoder: {codec:?} failed to start ({e:#}); using {next:?}");
                codec = next;
                build_pipe(codec, bitrate_kbps, frames.clone(), pushed.clone())?
            }
        };
        let encoder = Arc::new(Self {
            pipe: Mutex::new(pipe),
            codec: Mutex::new(codec),
            bitrate_kbps,
            frames,
            caps: Mutex::new(None),
            generation: AtomicU32::new(0),
            last: Mutex::new(None),
            last_keyframe_request: Mutex::new(None),
            epoch: Instant::now(),
            pushed,
        });
        let bus = encoder.pipe.lock().unwrap().pipeline.bus().unwrap();
        encoder.watch_bus(bus, 0)?;
        Ok(encoder)
    }

    /// Log the pipeline's messages. A bus error switches to the fallback encoder.
    /// The thread ends when the pipeline it watches is replaced or the encoder dropped.
    fn watch_bus(self: &Arc<Self>, bus: gst::Bus, generation: u32) -> Result<()> {
        let encoder = Arc::downgrade(self);
        std::thread::Builder::new().name("gst-bus".into()).spawn(move || loop {
            let msg = bus.timed_pop(gst::ClockTime::from_mseconds(500));
            let Some(this) = encoder.upgrade() else { return };
            if this.generation.load(Relaxed) != generation {
                return;
            }
            let Some(msg) = msg else { continue };
            match msg.view() {
                gst::MessageView::Error(e) => {
                    eprintln!("encoder: error from {:?}: {} ({:?})", e.src().map(|s| s.path_string()), e.error(), e.debug());
                    this.fall_back(generation);
                }
                gst::MessageView::Warning(w) => eprintln!("encoder: warning: {}", w.error()),
                gst::MessageView::Eos(_) => return,
                _ => {}
            }
        })?;
        Ok(())
    }

    /// Replace a failed pipeline (`generation`) with one using the fallback encoder,
    /// re-send the last frame so clients get a keyframe, and carry on. Does nothing if
    /// the pipeline was already replaced or there is no fallback (x264 failing is only
    /// logged: switching to itself would loop).
    fn fall_back(self: &Arc<Self>, generation: u32) {
        let mut pipe = self.pipe.lock().unwrap();
        if self.generation.load(Relaxed) != generation {
            return;
        }
        let failed = *self.codec.lock().unwrap();
        let Some(next) = fallback_codec(failed) else { return };
        eprintln!("encoder: {failed:?} failed, switching to {next:?}");
        let new = match build_pipe(next, self.bitrate_kbps, self.frames.clone(), self.pushed.clone()) {
            Ok(new) => new,
            Err(e) => {
                eprintln!("encoder: could not start {next:?}: {e:#}");
                return;
            }
        };
        let old = std::mem::replace(&mut *pipe, new);
        let _ = old.pipeline.set_state(gst::State::Null);
        *self.codec.lock().unwrap() = next;
        let generation = self.generation.fetch_add(1, Relaxed) + 1;
        if let Some(caps) = self.caps.lock().unwrap().as_ref() {
            pipe.appsrc.set_caps(Some(caps));
        }
        if let Some(last) = self.last.lock().unwrap().as_ref() {
            let mut buffer = last.copy();
            buffer.get_mut().unwrap().set_pts(self.now());
            let _ = pipe.appsrc.push_buffer(buffer);
        }
        if let Err(e) = self.watch_bus(pipe.pipeline.bus().unwrap(), generation) {
            eprintln!("encoder: cannot watch the new pipeline: {e:#}");
        }
    }

    /// The encoder in use now (`Nvenc` or `X264`); changes after a fallback.
    pub fn codec(&self) -> Codec {
        *self.codec.lock().unwrap()
    }

    pub fn set_format(&self, format: &str, width: u32, height: u32) {
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", format)
            .field("width", width as i32)
            .field("height", height as i32)
            // A variable rate (0/1) makes x264 hold back one frame, so the last
            // change before the screen goes static would never be sent.
            .field("framerate", gst::Fraction::new(60, 1))
            .build();
        let pipe = self.pipe.lock().unwrap();
        pipe.appsrc.set_caps(Some(&caps));
        *self.caps.lock().unwrap() = Some(caps);
    }

    pub fn push(&self, frame: Vec<u8>) {
        static FIRST: std::sync::Once = std::sync::Once::new();
        FIRST.call_once(|| eprintln!("encoder: first raw frame ({} bytes)", frame.len()));
        let mut buffer = gst::Buffer::from_mut_slice(frame);
        buffer.get_mut().unwrap().set_pts(self.now());
        *self.last.lock().unwrap() = Some(buffer.clone());
        self.pushed.fetch_add(1, Relaxed);
        let appsrc = self.pipe.lock().unwrap().appsrc.clone();
        let _ = appsrc.push_buffer(buffer);
    }

    fn now(&self) -> gst::ClockTime {
        gst::ClockTime::from_nseconds(self.epoch.elapsed().as_nanos() as u64)
    }

    /// Make the next encoded frame an IDR with SPS/PPS, re-sending the last
    /// frame so one is produced even if the screen doesn't change.
    pub fn request_keyframe(&self) {
        {
            let mut last = self.last_keyframe_request.lock().unwrap();
            if last.is_some_and(|t| t.elapsed() < MIN_KEYFRAME_INTERVAL) {
                return; // the pending keyframe serves this request too
            }
            *last = Some(Instant::now());
        }
        let (appsrc, appsink) = {
            let pipe = self.pipe.lock().unwrap();
            (pipe.appsrc.clone(), pipe.appsink.clone())
        };
        let event = gst_video::UpstreamForceKeyUnitEvent::builder().all_headers(true).build();
        appsink.send_event(event);
        if let Some(last) = self.last.lock().unwrap().as_ref() {
            let mut buffer = last.copy();
            buffer.get_mut().unwrap().set_pts(self.now());
            let _ = appsrc.push_buffer(buffer);
        }
    }

    pub fn stop(&self) {
        let pipe = self.pipe.lock().unwrap();
        self.generation.fetch_add(1, Relaxed);
        let _ = pipe.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_is_sized_from_the_bitrate() {
        let nv = encoder_description(Codec::Nvenc, 12000);
        assert!(nv.contains("rc-mode=cbr") && nv.contains("vbv-buffer-size=1800"), "{nv}");
        let x = encoder_description(Codec::X264, 6000);
        assert!(x.contains("pass=cbr") && x.contains("bitrate=6000") && x.contains("vbv-buf-capacity=150"), "{x}");
    }

    #[test]
    fn only_hardware_encoding_falls_back() {
        assert_eq!(fallback_codec(Codec::Nvenc), Some(Codec::X264));
        // x264 failing must not switch to itself, or the pipeline would restart forever.
        assert_eq!(fallback_codec(Codec::X264), None);
        assert_eq!(fallback_codec(Codec::Auto), None);
    }

    /// Needs GStreamer with x264enc; on a machine without nvh264enc this is the
    /// real "NVENC unavailable" case. Skipped where either assumption fails.
    #[test]
    fn missing_nvenc_starts_x264_and_encodes() {
        gst::init().unwrap();
        if gst::ElementFactory::find("x264enc").is_none() || gst::ElementFactory::find("nvh264enc").is_some() {
            return;
        }
        let (tx, mut rx) = broadcast::channel(8);
        let encoder = Encoder::new(Codec::Nvenc, 4000, tx).expect("falls back to x264");
        assert_eq!(encoder.codec(), Codec::X264);
        encoder.set_format("BGRx", 64, 64);
        for _ in 0..5 {
            encoder.push(vec![0x80; 64 * 64 * 4]);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let first = loop {
            match rx.try_recv() {
                Ok(frame) => break frame,
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => panic!("no encoded frame: {e}"),
            }
        };
        assert!(first.keyframe);
        encoder.stop();
    }
}
