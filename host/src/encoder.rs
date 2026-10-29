//! H.264 encoding with GStreamer: raw frames in through `appsrc`, Annex B
//! access units out through `appsink` onto a broadcast channel.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use tokio::sync::broadcast;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Codec {
    /// CPU (libx264)
    X264,
    /// NVIDIA NVENC
    Nvenc,
}

#[derive(Debug)]
pub struct EncodedFrame {
    pub keyframe: bool,
    pub pts_us: u64,
    pub data: Vec<u8>,
}

pub struct Encoder {
    pipeline: gst::Pipeline,
    appsrc: gst_app::AppSrc,
    appsink: gst_app::AppSink,
    /// Last raw frame, re-encoded as a keyframe when a client needs one while
    /// the screen is static (Mutter only sends frames on damage).
    last: Mutex<Option<gst::Buffer>>,
    last_keyframe_request: Mutex<Option<Instant>>,
}

/// Keyframes are ~10× the size of other frames; don't let clients ask for them back to back.
const MIN_KEYFRAME_INTERVAL: Duration = Duration::from_millis(250);

impl Encoder {
    pub fn new(codec: Codec, bitrate_kbps: u32, frames: broadcast::Sender<Arc<EncodedFrame>>) -> Result<Arc<Self>> {
        let encode = match codec {
            Codec::X264 => format!(
                "video/x-raw,format=I420 ! x264enc tune=zerolatency speed-preset=ultrafast bitrate={bitrate_kbps} \
                 key-int-max=600 bframes=0 sliced-threads=true"
            ),
            Codec::Nvenc => format!(
                "video/x-raw,format=NV12 ! nvh264enc preset=p1 tune=ultra-low-latency zerolatency=true bframes=0 \
                 bitrate={bitrate_kbps} gop-size=600"
            ),
        };
        let desc = format!(
            "appsrc name=src is-live=true format=time do-timestamp=true max-buffers=2 leaky-type=downstream \
             ! videoconvert n-threads=4 ! {encode} ! video/x-h264,profile=constrained-baseline \
             ! h264parse config-interval=-1 ! video/x-h264,stream-format=byte-stream,alignment=au \
             ! appsink name=sink sync=false max-buffers=4"
        );
        let pipeline = gst::parse::launch(&desc)
            .with_context(|| format!("building pipeline: {desc}"))?
            .downcast::<gst::Pipeline>()
            .unwrap();
        let appsrc = pipeline.by_name("src").unwrap().downcast::<gst_app::AppSrc>().unwrap();
        let appsink = pipeline.by_name("sink").unwrap().downcast::<gst_app::AppSink>().unwrap();

        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    static FIRST: std::sync::Once = std::sync::Once::new();
                    FIRST.call_once(|| eprintln!("encoder: first encoded frame ({} bytes)", map.len()));
                    let _ = frames.send(Arc::new(EncodedFrame {
                        keyframe: !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT),
                        pts_us: buffer.pts().map_or(0, |t| t.useconds()),
                        data: map.to_vec(),
                    }));
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        let bus = pipeline.bus().unwrap();
        std::thread::Builder::new().name("gst-bus".into()).spawn(move || {
            for msg in bus.iter_timed(gst::ClockTime::NONE) {
                match msg.view() {
                    gst::MessageView::Error(e) => {
                        eprintln!("encoder: error from {:?}: {} ({:?})", e.src().map(|s| s.path_string()), e.error(), e.debug())
                    }
                    gst::MessageView::Warning(w) => eprintln!("encoder: warning: {}", w.error()),
                    gst::MessageView::Eos(_) => break,
                    _ => {}
                }
            }
        })?;

        pipeline.set_state(gst::State::Playing).context("starting encoder pipeline")?;
        Ok(Arc::new(Self { pipeline, appsrc, appsink, last: Mutex::new(None), last_keyframe_request: Mutex::new(None) }))
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
        self.appsrc.set_caps(Some(&caps));
    }

    pub fn push(&self, frame: Vec<u8>) {
        static FIRST: std::sync::Once = std::sync::Once::new();
        FIRST.call_once(|| eprintln!("encoder: first raw frame ({} bytes)", frame.len()));
        let buffer = gst::Buffer::from_mut_slice(frame);
        *self.last.lock().unwrap() = Some(buffer.clone());
        let _ = self.appsrc.push_buffer(buffer);
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
        let event = gst_video::UpstreamForceKeyUnitEvent::builder().all_headers(true).build();
        self.appsink.send_event(event);
        if let Some(buffer) = self.last.lock().unwrap().clone() {
            let _ = self.appsrc.push_buffer(buffer);
        }
    }

    pub fn stop(&self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
