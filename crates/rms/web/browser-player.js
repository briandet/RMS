// "Play in this browser": the browser becomes a player of the server.  It connects to
// /api/v1/browser-player (a WebSocket, see crates/rms/src/bridge/browser.rs), plays what the
// server sends with <audio> elements, and reports what they do.  The server treats it like
// any Squeezebox: the queue, sync groups, alarms and every client's controls work on it.
//
// Lock-screen and media-key controls come from the Media Session API.

import { serverRoot, command, sizedArtwork } from './api.js';
import { store, on, emit } from './store.js';
import { storage, toast } from './util.js';

/** @typedef {import('./api-types').ToRenderer} ToRenderer */
/** @typedef {import('./api-types').FromRenderer} FromRenderer */

const ENABLED = 'rms.browserPlayer';
const ID = 'rms.browserId';
const NAME = 'rms.browserName';

/** The formats this browser's <audio> plays, best first (LMS codes). */
export function playableFormats() {
  const a = document.createElement('audio');
  const can = (t) => a.canPlayType(t) !== '';
  return [
    ['flc', 'audio/flac'],
    ['mp3', 'audio/mpeg'],
    ['ops', 'audio/ogg; codecs="opus"'],
    ['ogg', 'audio/ogg; codecs="vorbis"'],
    ['aac', 'audio/aac'],
  ].filter(([, t]) => can(t)).map(([f]) => f);
}

/** A name like "Firefox on Linux". */
export function defaultName() {
  const ua = navigator.userAgent;
  const browser = /Edg\//.test(ua) ? 'Edge' : /Firefox\//.test(ua) ? 'Firefox'
    : /Chrome\//.test(ua) ? 'Chrome' : /Safari\//.test(ua) ? 'Safari' : 'Browser';
  const os = /Android/.test(ua) ? 'Android' : /iPhone|iPad/.test(ua) ? 'iOS' : /Mac OS X/.test(ua) ? 'macOS'
    : /Windows/.test(ua) ? 'Windows' : /Linux/.test(ua) ? 'Linux' : '';
  return os ? `${browser} on ${os}` : browser;
}

function browserId() {
  let id = storage.get(ID);
  if (!id) {
    id = (crypto.randomUUID?.() || `${Date.now()}-${Math.random()}`).replace(/[^\w-]/g, '');
    storage.set(ID, id);
  }
  return id;
}

export const browserPlayer = {
  /** The server's id of this browser as a player, once connected. */
  playerId: null,
  enabled: () => storage.get(ENABLED) === '1',
  name: () => storage.get(NAME) || defaultName(),
  connected: () => !!socket && socket.readyState === WebSocket.OPEN && !!browserPlayer.playerId,
  /** Make it the selected player once it is connected (it was just turned on). */
  selectWhenConnected: false,
};

let socket = null;
let retry = 0;
let retryTimer = null;
// the playing element and the one holding the next track
let current = null;
let next = null;
let startedSent = false;
let lastProgress = 0;
let blocked = false;

function newAudio() {
  const a = new Audio();
  a.preload = 'auto';
  a.addEventListener('playing', () => { if (a === current) onPlaying(); });
  a.addEventListener('pause', () => { if (a === current && !a.ended && startedSent) report({ type: 'paused' }); });
  a.addEventListener('timeupdate', () => { if (a === current) progress(false); });
  a.addEventListener('ended', () => { if (a === current) onEnded(); });
  a.addEventListener('canplay', () => { if (a === current && !a.autoplay && !startedSent) report({ type: 'ready' }); });
  a.addEventListener('error', () => {
    if (a !== current || !a.getAttribute('src')) return;
    report({ type: 'error', message: a.error?.message || `media error ${a.error?.code || ''}`.trim() });
  });
  return a;
}

/** @param {FromRenderer} msg */
function report(msg) {
  if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(msg));
}

function progress(force) {
  const now = performance.now();
  if (!force && now - lastProgress < 1000) return;
  lastProgress = now;
  const d = current?.duration;
  report({ type: 'progress', elapsed: current?.currentTime || 0, ...(Number.isFinite(d) ? { duration: d } : {}) });
}

function onPlaying() {
  if (!startedSent) {
    startedSent = true;
    report({ type: 'started' });
  } else {
    report({ type: 'resumed' });
  }
  progress(true);
}

function onEnded() {
  if (next?.getAttribute('src')) {
    // the preloaded track takes over: gapless as far as the browser allows
    const old = current;
    current = next;
    next = old;
    next.removeAttribute('src');
    next.load();
    startedSent = false;
    play(current);
  } else {
    progress(true);
    report({ type: 'ended' });
  }
}

const absolute = (url) => serverRoot + url.replace(/^\/+/, '');

function play(a) {
  a.play().then(() => { blocked = false; }).catch((e) => {
    if (e.name === 'NotAllowedError') {
      // autoplay rules: wait for the next click or key on the page
      if (!blocked) toast('Click anywhere on the page to let this browser play', 'error');
      blocked = true;
      const resume = () => { window.removeEventListener('pointerdown', resume, true); window.removeEventListener('keydown', resume, true); if (a === current) play(a); };
      window.addEventListener('pointerdown', resume, true);
      window.addEventListener('keydown', resume, true);
    } else if (e.name !== 'AbortError') {
      report({ type: 'error', message: e.message });
    }
  });
}

/** @param {ToRenderer} msg */
function handle(msg) {
  switch (msg.type) {
    case 'load':
      startedSent = false;
      next.removeAttribute('src');
      current.autoplay = !!msg.autostart;
      current.src = absolute(msg.url);
      if (msg.autostart) play(current);
      else current.load();
      break;
    case 'preload':
      next.src = absolute(msg.url);
      next.load();
      break;
    case 'pause':
      current.pause();
      break;
    case 'resume':
      if (current.getAttribute('src')) play(current);
      break;
    case 'stop':
      startedSent = false;
      for (const a of [current, next]) { a.pause(); a.removeAttribute('src'); a.load(); }
      break;
    case 'volume': {
      // the server's 0–100 is roughly logarithmic; the element's 0–1 is linear
      const v = Math.max(0, Math.min(100, msg.volume)) / 100;
      for (const a of [current, next]) { a.volume = v * v; a.muted = !!msg.muted; }
      break;
    }
    default:
      break;
  }
}

function connect() {
  clearTimeout(retryTimer);
  if (!browserPlayer.enabled() || socket) return;
  current ||= newAudio();
  next ||= newAudio();
  const base = new URL(serverRoot, location.href);
  base.protocol = base.protocol === 'https:' ? 'wss:' : 'ws:';
  const q = new URLSearchParams({ id: browserId(), name: browserPlayer.name(), formats: playableFormats().join(',') });
  const ws = new WebSocket(`${base.href}api/v1/browser-player?${q}`);
  socket = ws;
  ws.addEventListener('message', (e) => {
    let msg;
    try { msg = JSON.parse(e.data); } catch { return; }
    if (msg.type === 'welcome') {
      browserPlayer.playerId = msg.player;
      retry = 0;
      emit('browser-player', browserPlayer);
      return;
    }
    handle(msg);
  });
  ws.addEventListener('close', () => {
    if (socket !== ws) return;
    socket = null;
    browserPlayer.playerId = null;
    emit('browser-player', browserPlayer);
    if (browserPlayer.enabled()) {
      retry = Math.min(retry + 1, 6);
      retryTimer = setTimeout(connect, 500 * 2 ** retry);
    }
  });
}

function disconnect() {
  clearTimeout(retryTimer);
  const ws = socket;
  socket = null;
  browserPlayer.playerId = null;
  ws?.close();
  for (const a of [current, next]) { if (a) { a.pause(); a.removeAttribute('src'); a.load(); } }
  emit('browser-player', browserPlayer);
}

/** Turn "play in this browser" on or off (call from a click: browsers only play after one). */
export function setBrowserPlayer(on, name) {
  if (name !== undefined) storage.set(NAME, name.trim());
  storage.set(ENABLED, on ? '1' : '0');
  if (on) {
    browserPlayer.selectWhenConnected = true;
    current ||= newAudio();
    next ||= newAudio();
    // a click is what lets the page play later on
    current.play().catch(() => {});
    disconnect();
    connect();
  } else {
    disconnect();
  }
}

// ------------------------------------------------------------------ media session

function mediaSession() {
  const ms = navigator.mediaSession;
  if (!ms) return;
  const pid = () => browserPlayer.playerId;
  const act = (body) => () => { if (pid()) command(pid(), body).catch(() => {}); };
  const handlers = {
    play: act({ command: 'play' }),
    pause: act({ command: 'pause' }),
    stop: act({ command: 'stop' }),
    nexttrack: act({ command: 'next' }),
    previoustrack: act({ command: 'previous' }),
    seekto: (d) => { if (pid() && Number.isFinite(d.seekTime)) command(pid(), { command: 'seek', position: d.seekTime }).catch(() => {}); },
  };
  for (const [k, fn] of Object.entries(handlers)) {
    try { ms.setActionHandler(k, fn); } catch { /* not supported here */ }
  }
  let key = '';
  const update = () => {
    const p = store.players.find((x) => x.id === pid());
    const t = p?.now_playing;
    const k = t ? `${t.title}|${t.artist}|${t.album}|${t.artwork}` : '';
    if (k !== key) {
      key = k;
      const art = t?.artwork ? sizedArtwork(t.artwork, 512) : null;
      ms.metadata = t ? new MediaMetadata({
        title: t.title || '', artist: t.artist || t.station || '', album: t.album || '',
        artwork: art ? [{ src: art, sizes: '512x512' }] : [],
      }) : null;
    }
    ms.playbackState = !p ? 'none' : p.mode === 'play' ? 'playing' : p.mode === 'pause' ? 'paused' : 'none';
  };
  on('player-state', update);
  on('browser-player', update);
}

/** Start: reconnect when it was on, and keep the lock screen current. */
export function initBrowserPlayer() {
  mediaSession();
  if (browserPlayer.enabled()) connect();
}
