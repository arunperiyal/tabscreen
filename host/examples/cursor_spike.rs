//! M0 follow-up: does a direct PipeWire consumer get cursor updates at full rate?
//!
//! Creates a virtual monitor with cursor-mode=metadata, moves the pointer in a
//! circle on it at ~120 Hz, and counts per second: frames with pixel data,
//! cursor position updates and cursor bitmap updates.
//!
//!   cargo run --example cursor_spike -- [WIDTHxHEIGHT] [SECONDS]

use std::f64::consts::TAU;
use std::mem::size_of;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use anyhow::Result;
use pipewire as pw;
use pw::spa;
use pw::spa::pod::{ChoiceValue, Object, Pod, Property, Value};
use pw::spa::sys as spa_sys;
use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Rectangle};
use tabscreen_host::mutter::{CursorMode, VirtualMonitor};

#[derive(Default)]
struct Stats {
    frames: AtomicU64,
    empty: AtomicU64,
    cursor_moves: AtomicU64,
    bitmaps: AtomicU64,
    x: AtomicI64,
    y: AtomicI64,
}

const fn cursor_meta_size(w: usize, h: usize) -> i32 {
    (size_of::<spa_sys::spa_meta_cursor>() + size_of::<spa_sys::spa_meta_bitmap>() + w * h * 4) as i32
}

fn serialize(obj: Object) -> Vec<u8> {
    pw::spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
        .unwrap()
        .0
        .into_inner()
}

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

/// Read cursor metadata and frame presence from one buffer.
unsafe fn inspect(buf: *mut spa_sys::spa_buffer, stats: &Stats, first_bitmap: &AtomicBool) {
    let buf = unsafe { &*buf };
    for i in 0..buf.n_metas as usize {
        let meta = unsafe { &*buf.metas.add(i) };
        if meta.type_ != spa_sys::SPA_META_Cursor || meta.size < size_of::<spa_sys::spa_meta_cursor>() as u32 {
            continue;
        }
        let cursor = unsafe { &*(meta.data as *const spa_sys::spa_meta_cursor) };
        if cursor.id == 0 {
            continue;
        }
        stats.cursor_moves.fetch_add(1, Relaxed);
        stats.x.store(cursor.position.x as i64, Relaxed);
        stats.y.store(cursor.position.y as i64, Relaxed);
        if cursor.bitmap_offset as usize >= size_of::<spa_sys::spa_meta_cursor>() {
            let bitmap = unsafe {
                &*((meta.data as *const u8).add(cursor.bitmap_offset as usize) as *const spa_sys::spa_meta_bitmap)
            };
            if bitmap.format != 0 {
                stats.bitmaps.fetch_add(1, Relaxed);
                if !first_bitmap.swap(true, Relaxed) {
                    println!(
                        "cursor bitmap: {}x{} format={} stride={} hotspot=({},{})",
                        bitmap.size.width,
                        bitmap.size.height,
                        bitmap.format,
                        bitmap.stride,
                        cursor.hotspot.x,
                        cursor.hotspot.y
                    );
                }
            }
        }
    }
    let has_pixels = buf.n_datas > 0 && {
        let chunk = unsafe { &*(*buf.datas).chunk };
        chunk.size > 0 && chunk.flags & spa_sys::SPA_CHUNK_FLAG_CORRUPTED as i32 == 0
    };
    if has_pixels { &stats.frames } else { &stats.empty }.fetch_add(1, Relaxed);
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (width, height) = args
        .next()
        .and_then(|s| s.split_once('x').map(|(w, h)| (w.parse().unwrap(), h.parse().unwrap())))
        .unwrap_or((1600u32, 1000u32));
    let seconds: u64 = args.next().map(|s| s.parse().unwrap()).unwrap_or(6);

    let vm = Arc::new(VirtualMonitor::start(CursorMode::Metadata)?);
    println!("virtual monitor node {} ({width}x{height}, cursor-mode metadata)", vm.node_id);

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "tabscreen-cursor-spike",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let stats = Arc::new(Stats::default());
    let first_bitmap = AtomicBool::new(false);
    let process_stats = stats.clone();
    let meta = cursor_meta_param();
    let _listener = stream
        .add_local_listener_with_user_data(())
        .param_changed(move |stream, _, id, param| {
            if id != spa::param::ParamType::Format.as_raw() || param.is_none() {
                return;
            }
            let mut info = spa::param::video::VideoInfoRaw::default();
            if info.parse(param.unwrap()).is_ok() {
                println!(
                    "negotiated {:?} {}x{} max {}/{} fps",
                    info.format(),
                    info.size().width,
                    info.size().height,
                    info.max_framerate().num,
                    info.max_framerate().denom
                );
            }
            let mut params = [Pod::from_bytes(&meta).unwrap()];
            if let Err(e) = stream.update_params(&mut params) {
                eprintln!("update_params: {e}");
            }
        })
        .process(move |stream, _| unsafe {
            let buf = stream.dequeue_raw_buffer();
            if buf.is_null() {
                return;
            }
            inspect((*buf).buffer, &process_stats, &first_bitmap);
            stream.queue_raw_buffer(buf);
        })
        .register()?;

    let format = format_param(width, height);
    stream.connect(
        spa::utils::Direction::Input,
        Some(vm.node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut [Pod::from_bytes(&format).unwrap()],
    )?;

    // Move the pointer in a circle on the virtual monitor at ~120 Hz.
    let mover = {
        let vm = vm.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(seconds) {
                let a = t0.elapsed().as_secs_f64() * TAU / 2.0;
                let (cx, cy) = (width as f64 / 2.0, height as f64 / 2.0);
                let _ = vm.pointer_motion_absolute(cx + cx * 0.6 * a.cos(), cy + cy * 0.6 * a.sin());
                std::thread::sleep(Duration::from_millis(8));
            }
        })
    };

    let weak = mainloop.downgrade();
    let tick = std::cell::Cell::new(0u64);
    let timer = mainloop.loop_().add_timer(move |_| {
        let n = tick.get() + 1;
        tick.set(n);
        println!(
            "t={n:>2}s frames={:>3} empty={:>3} cursor_moves={:>3} bitmaps={} pos=({},{})",
            stats.frames.swap(0, Relaxed),
            stats.empty.swap(0, Relaxed),
            stats.cursor_moves.swap(0, Relaxed),
            stats.bitmaps.swap(0, Relaxed),
            stats.x.load(Relaxed),
            stats.y.load(Relaxed)
        );
        if n >= seconds + 3 {
            if let Some(ml) = weak.upgrade() {
                ml.quit();
            }
        }
    });
    timer.update_timer(Some(Duration::from_secs(1)), Some(Duration::from_secs(1))).into_result()?;

    mainloop.run();
    mover.join().unwrap();
    vm.stop();
    Ok(())
}
