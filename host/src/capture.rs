//! PipeWire consumer for the virtual monitor's stream.
//!
//! Frames with pixel data go to the encoder; `SPA_META_Cursor` updates go to
//! the cursor channels. Cursor-only buffers are why this isn't `pipewiresrc`:
//! it drops them, see PLAN.md (M0 follow-up).

use std::mem::size_of;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use pipewire as pw;
use pw::spa;
use pw::spa::pod::{ChoiceValue, Object, Pod, Property, Value};
use pw::spa::sys as spa_sys;
use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Rectangle};
use tokio::sync::watch;

use crate::encoder::Encoder;

/// Cursor image in RGBA. `width == 0` means the cursor is hidden.
#[derive(Debug, Default)]
pub struct CursorImage {
    pub width: u16,
    pub height: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    pub rgba: Vec<u8>,
}

pub struct CursorChannels {
    /// Cursor position in monitor pixels.
    pub position: watch::Sender<(i32, i32)>,
    pub image: watch::Sender<Arc<CursorImage>>,
}

const fn cursor_meta_size(w: usize, h: usize) -> i32 {
    (size_of::<spa_sys::spa_meta_cursor>() + size_of::<spa_sys::spa_meta_bitmap>() + w * h * 4) as i32
}

fn serialize(obj: Object) -> Vec<u8> {
    pw::spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
        .expect("serialize pod")
        .0
        .into_inner()
}

/// Packed RGB formats we accept; the fixed size makes Mutter size the monitor.
fn format_param(width: u32, height: u32) -> Vec<u8> {
    use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use spa::param::video::VideoFormat;
    serialize(pw::spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        pw::spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pw::spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pw::spa::pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA
        ),
        pw::spa::pod::property!(FormatProperties::VideoSize, Rectangle, Rectangle { width, height }),
        pw::spa::pod::property!(FormatProperties::VideoFramerate, Fraction, Fraction { num: 0, denom: 1 }),
        pw::spa::pod::property!(
            FormatProperties::VideoMaxFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: 60, denom: 1 },
            Fraction { num: 1, denom: 1 },
            Fraction { num: 60, denom: 1 }
        ),
    ))
}

fn cursor_meta_param() -> Vec<u8> {
    serialize(Object {
        type_: spa_sys::SPA_TYPE_OBJECT_ParamMeta,
        id: spa_sys::SPA_PARAM_Meta,
        properties: vec![
            Property::new(spa_sys::SPA_PARAM_META_type, Value::Id(spa::utils::Id(spa_sys::SPA_META_Cursor))),
            Property::new(
                spa_sys::SPA_PARAM_META_size,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: cursor_meta_size(64, 64),
                        min: cursor_meta_size(1, 1),
                        max: cursor_meta_size(384, 384),
                    },
                ))),
            ),
        ],
    })
}

fn gst_format_name(format: spa::param::video::VideoFormat) -> Option<&'static str> {
    use spa::param::video::VideoFormat;
    Some(match format {
        VideoFormat::BGRx => "BGRx",
        VideoFormat::BGRA => "BGRA",
        VideoFormat::RGBx => "RGBx",
        VideoFormat::RGBA => "RGBA",
        _ => return None,
    })
}

/// Convert a cursor bitmap to RGBA, or `None` for formats we don't handle.
fn bitmap_to_rgba(format: u32, width: usize, height: usize, stride: usize, src: *const u8) -> Option<Vec<u8>> {
    // Byte positions of R, G, B, A within a pixel.
    let order: [usize; 4] = match format {
        spa_sys::SPA_VIDEO_FORMAT_RGBA => [0, 1, 2, 3],
        spa_sys::SPA_VIDEO_FORMAT_BGRA => [2, 1, 0, 3],
        spa_sys::SPA_VIDEO_FORMAT_ARGB => [1, 2, 3, 0],
        spa_sys::SPA_VIDEO_FORMAT_ABGR => [3, 2, 1, 0],
        _ => return None,
    };
    let mut rgba = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let line = unsafe { std::slice::from_raw_parts(src.add(row * stride), width * 4) };
        for px in line.chunks_exact(4) {
            rgba.extend(order.iter().map(|&i| px[i]));
        }
    }
    Some(rgba)
}

fn read_cursor(buf: &spa_sys::spa_buffer, cursor: &CursorChannels) {
    for i in 0..buf.n_metas as usize {
        let meta = unsafe { &*buf.metas.add(i) };
        if meta.type_ != spa_sys::SPA_META_Cursor || meta.size < size_of::<spa_sys::spa_meta_cursor>() as u32 {
            continue;
        }
        let c = unsafe { &*(meta.data as *const spa_sys::spa_meta_cursor) };
        if c.id == 0 {
            continue; // no new cursor data
        }
        cursor.position.send_if_modified(|pos| {
            let new = (c.position.x, c.position.y);
            std::mem::replace(pos, new) != new
        });
        if (c.bitmap_offset as usize) < size_of::<spa_sys::spa_meta_cursor>() {
            continue; // position-only update
        }
        let base = unsafe { (meta.data as *const u8).add(c.bitmap_offset as usize) };
        let bitmap = unsafe { &*(base as *const spa_sys::spa_meta_bitmap) };
        let image = if bitmap.format == 0 || (bitmap.offset as usize) < size_of::<spa_sys::spa_meta_bitmap>() {
            CursorImage::default()
        } else {
            let (w, h) = (bitmap.size.width as usize, bitmap.size.height as usize);
            match bitmap_to_rgba(bitmap.format, w, h, bitmap.stride as usize, unsafe { base.add(bitmap.offset as usize) }) {
                Some(rgba) => CursorImage {
                    width: w as u16,
                    height: h as u16,
                    hot_x: c.hotspot.x.max(0) as u16,
                    hot_y: c.hotspot.y.max(0) as u16,
                    rgba,
                },
                None => {
                    eprintln!("capture: unsupported cursor bitmap format {}", bitmap.format);
                    continue;
                }
            }
        };
        cursor.image.send_replace(Arc::new(image));
    }
}

/// Copy the frame's pixels into a tightly packed buffer, if the buffer has any.
fn read_frame(buf: &spa_sys::spa_buffer, width: usize, height: usize) -> Option<Vec<u8>> {
    if buf.n_datas == 0 {
        return None;
    }
    let data = unsafe { &*buf.datas };
    let chunk = unsafe { &*data.chunk };
    if data.data.is_null() || chunk.size == 0 || chunk.flags & spa_sys::SPA_CHUNK_FLAG_CORRUPTED as i32 != 0 {
        return None; // cursor-only buffer
    }
    let row = width * 4;
    let stride = if chunk.stride > 0 { chunk.stride as usize } else { row };
    if (chunk.offset as usize) + stride * (height - 1) + row > data.maxsize as usize {
        eprintln!("capture: frame smaller than {width}x{height}, skipping");
        return None;
    }
    let src = unsafe { (data.data as *const u8).add(chunk.offset as usize) };
    let mut out = Vec::with_capacity(row * height);
    for y in 0..height {
        out.extend_from_slice(unsafe { std::slice::from_raw_parts(src.add(y * stride), row) });
    }
    Some(out)
}

/// Frame rate from PipeWire and time spent copying, logged every few seconds.
struct Stats {
    since: Instant,
    frames: u32,
    copy: Duration,
}

impl Stats {
    fn new() -> Self {
        Self { since: Instant::now(), frames: 0, copy: Duration::ZERO }
    }

    fn add(&mut self, copy: Duration) {
        self.frames += 1;
        self.copy += copy;
        let elapsed = self.since.elapsed();
        if elapsed >= Duration::from_secs(5) {
            eprintln!(
                "capture: {:.0} fps from PipeWire, copy avg {:.1} ms",
                self.frames as f64 / elapsed.as_secs_f64(),
                self.copy.as_secs_f64() * 1000.0 / self.frames as f64
            );
            *self = Self::new();
        }
    }
}

/// Run the PipeWire consumer on its own thread until the stream goes away.
pub fn spawn(node_id: u32, width: u32, height: u32, encoder: Arc<Encoder>, cursor: CursorChannels) -> JoinHandle<Result<()>> {
    std::thread::Builder::new()
        .name("pipewire".into())
        .spawn(move || run(node_id, width, height, encoder, cursor))
        .expect("spawn pipewire thread")
}

fn run(node_id: u32, width: u32, height: u32, encoder: Arc<Encoder>, cursor: CursorChannels) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "tabscreen",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let meta = cursor_meta_param();
    let format_encoder = encoder.clone();
    let weak = mainloop.downgrade();
    let _listener = stream
        .add_local_listener_with_user_data(((0usize, 0usize), Stats::new()))
        .state_changed(move |_, _, old, new| {
            eprintln!("capture: stream {old:?} -> {new:?}");
            if matches!(new, pw::stream::StreamState::Unconnected | pw::stream::StreamState::Error(_)) {
                if let Some(ml) = weak.upgrade() {
                    ml.quit();
                }
            }
        })
        .param_changed(move |stream, size, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let mut info = spa::param::video::VideoInfoRaw::default();
            if info.parse(param).is_err() {
                return;
            }
            let (w, h) = (info.size().width, info.size().height);
            let Some(name) = gst_format_name(info.format()) else {
                eprintln!("capture: unexpected format {:?}", info.format());
                return;
            };
            eprintln!("capture: negotiated {name} {w}x{h}");
            size.0 = (w as usize, h as usize);
            format_encoder.set_format(name, w, h);
            let mut params = [Pod::from_bytes(&meta).unwrap()];
            if let Err(e) = stream.update_params(&mut params) {
                eprintln!("capture: update_params: {e}");
            }
        })
        .process(move |stream, ((w, h), stats)| unsafe {
            let (w, h) = (*w, *h);
            let pw_buf = stream.dequeue_raw_buffer();
            if pw_buf.is_null() {
                return;
            }
            let buf = &*(*pw_buf).buffer;
            read_cursor(buf, &cursor);
            if w > 0 {
                let start = Instant::now();
                if let Some(frame) = read_frame(buf, w, h) {
                    stats.add(start.elapsed());
                    encoder.push(frame);
                }
            }
            stream.queue_raw_buffer(pw_buf);
        })
        .register()?;

    let format = format_param(width, height);
    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut [Pod::from_bytes(&format).unwrap()],
    )?;
    mainloop.run();
    Ok(())
}
