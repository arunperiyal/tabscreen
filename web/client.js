// TabScreen web client: decodes the H.264 stream with WebCodecs, draws the
// cursor on an overlay, and sends touches back. Protocol: see PLAN.md.
'use strict';

const MSG = { HELLO: 1, CONFIG: 2, FRAME: 3, TOUCH: 4, KEYFRAME_REQUEST: 5, PING: 6, CURSOR_MOVE: 7, CURSOR_IMAGE: 8 };
const TOUCH = { DOWN: 0, MOVE: 1, UP: 2, CANCEL: 3 };

const screenCanvas = document.getElementById('screen');
const cursorCanvas = document.getElementById('cursor');
const statusEl = document.getElementById('status');
const startEl = document.getElementById('start');
const screenCtx = screenCanvas.getContext('2d');
const cursorCtx = cursorCanvas.getContext('2d');

let ws = null;
let decoder = null;
let decoderState = 'none'; // none | configuring | ready | unsupported
let lastKeyframeRequest = 0;
let size = { w: 0, h: 0 };
let cursor = { x: 0, y: 0, image: null, hotX: 0, hotY: 0, dirty: true };
let stats = { frames: 0, bytes: 0, rtt: null, decodeMs: 0 };
const decodeStart = new Map(); // chunk timestamp -> performance.now() at decode()

function setStatus(text) { statusEl.textContent = text; }

function send(bytes) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(bytes);
}

// Throttled, so a failing decoder can't turn the stream into a keyframe storm.
function requestKeyframe() {
  const now = performance.now();
  if (now - lastKeyframeRequest < 500) return;
  lastKeyframeRequest = now;
  send(new Uint8Array([MSG.KEYFRAME_REQUEST]));
}

// --- layout -----------------------------------------------------------------

function layout() {
  if (!size.w) return;
  const scale = Math.min(innerWidth / size.w, innerHeight / size.h);
  const w = size.w * scale, h = size.h * scale;
  for (const c of [screenCanvas, cursorCanvas]) {
    c.style.width = `${w}px`;
    c.style.height = `${h}px`;
    c.style.left = `${(innerWidth - w) / 2}px`;
    c.style.top = `${(innerHeight - h) / 2}px`;
  }
}
addEventListener('resize', layout);

// --- video --------------------------------------------------------------------

// Codec string from the SPS: avc1.PPCCLL (profile, constraint flags, level).
function codecFromAnnexB(data) {
  for (let i = 0; i + 4 < data.length; i++) {
    if (data[i] === 0 && data[i + 1] === 0 && data[i + 2] === 1 && (data[i + 3] & 0x1f) === 7) {
      const hex = (b) => b.toString(16).padStart(2, '0');
      return `avc1.${hex(data[i + 4])}${hex(data[i + 5])}${hex(data[i + 6])}`;
    }
  }
  return null;
}

function resetDecoder() {
  if (decoder && decoder.state !== 'closed') decoder.close();
  decoder = null;
  if (decoderState !== 'unsupported') decoderState = 'none';
}

// Prefer the hardware decoder (tablets have one); fall back to software
// (desktop Chrome on Linux often has no hardware H.264 decoder).
async function setupDecoder(codec, firstChunk) {
  decoderState = 'configuring';
  for (const hardwareAcceleration of ['prefer-hardware', 'no-preference']) {
    // No `description`: the stream is Annex B with SPS/PPS in-band.
    const config = { codec, optimizeForLatency: true, hardwareAcceleration };
    const { supported } = await VideoDecoder.isConfigSupported(config);
    if (!supported) continue;
    decoder = new VideoDecoder({
      output: (frame) => {
        const start = decodeStart.get(frame.timestamp);
        if (start !== undefined) {
          stats.decodeMs += performance.now() - start;
          decodeStart.delete(frame.timestamp);
        }
        screenCtx.drawImage(frame, 0, 0, size.w, size.h);
        frame.close();
        stats.frames++;
      },
      error: (e) => {
        console.error('decoder error', e);
        resetDecoder();
        requestKeyframe();
      },
    });
    decoder.configure(config);
    decoderState = 'ready';
    decodeStart.set(firstChunk.timestamp, performance.now());
    decoder.decode(firstChunk); // the keyframe that triggered setup
    console.log('decoder configured', codec, hardwareAcceleration);
    return;
  }
  decoderState = 'unsupported';
  setStatus(`this browser can't decode H.264 ${codec}`);
}

function onFrame(view, buffer) {
  const keyframe = (view.getUint8(1) & 1) === 1;
  const pts = Number(view.getBigUint64(2, true));
  const data = new Uint8Array(buffer, 10);
  stats.bytes += data.length;

  const chunk = new EncodedVideoChunk({ type: keyframe ? 'key' : 'delta', timestamp: pts, data });
  if (decoderState === 'none' && keyframe) {
    const codec = codecFromAnnexB(data);
    if (codec) setupDecoder(codec, chunk);
    else console.warn('keyframe without SPS');
    return;
  }
  if (decoderState !== 'ready') return;
  // Falling behind: drop deltas and resync at the next keyframe.
  if (decoder.decodeQueueSize > 4 && !keyframe) {
    resetDecoder();
    requestKeyframe();
    return;
  }
  if (decodeStart.size > 100) decodeStart.clear(); // frames dropped by a decoder reset
  decodeStart.set(chunk.timestamp, performance.now());
  decoder.decode(chunk);
}

// --- cursor -------------------------------------------------------------------

async function onCursorImage(view, buffer) {
  const w = view.getUint16(1, true), h = view.getUint16(3, true);
  if (w === 0) {
    cursor.image = null;
  } else {
    const pixels = new Uint8ClampedArray(buffer, 9, w * h * 4);
    cursor.image = await createImageBitmap(new ImageData(new Uint8ClampedArray(pixels), w, h));
    cursor.hotX = view.getUint16(5, true);
    cursor.hotY = view.getUint16(7, true);
  }
  cursor.dirty = true;
}

function drawCursor() {
  if (cursor.dirty) {
    cursor.dirty = false;
    cursorCtx.clearRect(0, 0, cursorCanvas.width, cursorCanvas.height);
    if (cursor.image && cursor.x >= 0 && cursor.y >= 0 && cursor.x < size.w && cursor.y < size.h) {
      cursorCtx.drawImage(cursor.image, cursor.x - cursor.hotX, cursor.y - cursor.hotY);
    }
  }
  requestAnimationFrame(drawCursor);
}
requestAnimationFrame(drawCursor);

// --- touch ----------------------------------------------------------------------

const slots = new Map(); // pointerId -> slot

function sendTouch(action, e) {
  let slot = slots.get(e.pointerId);
  if (slot === undefined) {
    if (action !== TOUCH.DOWN) return;
    const used = new Set(slots.values());
    for (slot = 0; used.has(slot); slot++);
    slots.set(e.pointerId, slot);
  }
  if (action === TOUCH.UP || action === TOUCH.CANCEL) slots.delete(e.pointerId);

  const rect = cursorCanvas.getBoundingClientRect();
  const buf = new ArrayBuffer(11);
  const view = new DataView(buf);
  view.setUint8(0, MSG.TOUCH);
  view.setUint8(1, action);
  view.setUint8(2, slot);
  view.setFloat32(3, (e.clientX - rect.left) / rect.width, true);
  view.setFloat32(7, (e.clientY - rect.top) / rect.height, true);
  send(buf);
}

cursorCanvas.addEventListener('pointerdown', (e) => {
  // Keep receiving moves when a finger slides off the canvas edge.
  try { cursorCanvas.setPointerCapture(e.pointerId); } catch { /* pointer already gone */ }
  sendTouch(TOUCH.DOWN, e);
});
cursorCanvas.addEventListener('pointermove', (e) => {
  for (const ev of e.getCoalescedEvents ? e.getCoalescedEvents() : [e]) sendTouch(TOUCH.MOVE, ev);
});
cursorCanvas.addEventListener('pointerup', (e) => sendTouch(TOUCH.UP, e));
cursorCanvas.addEventListener('pointercancel', (e) => sendTouch(TOUCH.CANCEL, e));
cursorCanvas.addEventListener('contextmenu', (e) => e.preventDefault());

// --- connection -------------------------------------------------------------------

function connect() {
  ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`);
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => {
    const hello = JSON.stringify({
      w: Math.round(screen.width * devicePixelRatio),
      h: Math.round(screen.height * devicePixelRatio),
      dpr: devicePixelRatio,
      codecs: ['h264'],
      version: 1,
    });
    send(new Uint8Array([MSG.HELLO, ...new TextEncoder().encode(hello)]));
    setStatus('connected, waiting for video…');
  };
  ws.onmessage = (ev) => {
    const buffer = ev.data;
    const view = new DataView(buffer);
    switch (view.getUint8(0)) {
      case MSG.CONFIG: {
        const config = JSON.parse(new TextDecoder().decode(new Uint8Array(buffer, 1)));
        size = { w: config.w, h: config.h };
        screenCanvas.width = cursorCanvas.width = size.w;
        screenCanvas.height = cursorCanvas.height = size.h;
        cursor.dirty = true;
        layout();
        break;
      }
      case MSG.FRAME: onFrame(view, buffer); break;
      case MSG.CURSOR_MOVE:
        cursor.x = view.getInt32(1, true);
        cursor.y = view.getInt32(5, true);
        cursor.dirty = true;
        break;
      case MSG.CURSOR_IMAGE: onCursorImage(view, buffer); break;
      case MSG.PING:
        stats.rtt = performance.now() - Number(view.getBigUint64(1, true)) / 1000;
        break;
    }
  };
  ws.onclose = () => {
    resetDecoder();
    slots.clear();
    setStatus('disconnected, retrying…');
    setTimeout(connect, 1000);
  };
}

setInterval(() => {
  // Frames are skipped until a keyframe; keep asking in case a request was dropped.
  if (decoderState === 'none') requestKeyframe();
  const ping = new DataView(new ArrayBuffer(9));
  ping.setUint8(0, MSG.PING);
  ping.setBigUint64(1, BigInt(Math.round(performance.now() * 1000)), true);
  send(ping.buffer);
  if (decoderState === 'ready') {
    const rtt = stats.rtt === null ? '–' : `${stats.rtt.toFixed(0)} ms`;
    const decode = stats.frames ? `${(stats.decodeMs / stats.frames).toFixed(0)} ms` : '–';
    setStatus(`${size.w}×${size.h} · ${stats.frames} fps · ${(stats.bytes * 8 / 1e6).toFixed(1)} Mbit/s · rtt ${rtt} · decode ${decode}`);
  }
  stats.frames = 0;
  stats.bytes = 0;
  stats.decodeMs = 0;
}, 1000);

startEl.addEventListener('click', async () => {
  startEl.remove();
  try { await document.documentElement.requestFullscreen({ navigationUI: 'hide' }); } catch (e) { console.warn(e); }
  try { await navigator.wakeLock.request('screen'); } catch (e) { console.warn(e); }
});

if (!('VideoDecoder' in window)) {
  setStatus('WebCodecs unavailable: open via http://localhost (USB) or HTTPS');
} else {
  connect();
}
