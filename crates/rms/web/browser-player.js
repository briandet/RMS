// "Play in this browser": the browser becomes a player of the server.  It connects to
// /api/v1/browser-player (a WebSocket, see crates/rms/src/bridge/browser.rs), plays what the
// server sends, and reports what it does.  The server treats it like any Squeezebox: the
// queue, sync groups, alarms and every client's controls work on it.
//
// Two ways to play:
// - Web Audio: the server sends FLAC, or Opus to save data, which the browser decodes with
//   WebCodecs (decoders.js; raw samples where it has no WebCodecs, as on pages that aren't
//   https), and an AudioWorklet (pcm-worklet.js; without one a ScriptProcessorNode) plays
//   them on the audio clock.  The browser reports its
//   clock and exactly where playback is on it, starts at the moment it is told and plays a
//   little ahead or behind when told, so it keeps in step with the players it is synced with
//   the way a Squeezebox does.  Tracks follow each other without a gap.
// - <audio> elements: any format the browser plays, but only roughly in step.
//
// Lock-screen and media-key controls come from the Media Session API.

import { serverRoot, command, sizedArtwork } from './api.js';
import { store, on, emit } from './store.js';
import { storage, toast } from './util.js';
import { createWebAudioPlayer } from './web-audio-player.js';

/** @typedef {import('./api-types').ToRenderer} ToRenderer */
/** @typedef {import('./api-types').FromRenderer} FromRenderer */

const ENABLED = 'rms.browserPlayer';
const PRECISE = 'rms.browserPrecise';
const QUALITY = 'rms.browserQuality';
const ID = 'rms.browserId';
const NAME = 'rms.browserName';
/** In this tab's session storage: this page is the one playing (so a reload keeps it). */
const HERE = 'rms.browserPlayer.here';
/** How often a page waiting for the player (another page has it) looks whether it's free. */
const ELSEWHERE_RETRY = 10000;

const session = {
  get: () => { try { return sessionStorage.getItem(HERE) === '1'; } catch { return false; } },
  set: (on) => { try { if (on) sessionStorage.setItem(HERE, '1'); else sessionStorage.removeItem(HERE); } catch { /* private mode */ } },
};

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

/** Whether this browser can play through Web Audio (and so keep in sync). */
export function webAudioSupported() {
  return typeof AudioContext !== 'undefined' && typeof ReadableStream !== 'undefined';
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
  /** Plays through Web Audio, in sync (unless turned off, or not supported). */
  precise: () => webAudioSupported() && storage.get(PRECISE) !== '0',
  /** How Web Audio gets its audio: `lossless` (FLAC) or `compact` (Opus, for slow networks). */
  quality: () => (storage.get(QUALITY) === 'compact' ? 'compact' : 'lossless'),
  /** What it decodes, once known: LMS codes (`flc`, `ops`; `pcm` always). */
  decodes: [],
  name: () => storage.get(NAME) || defaultName(),
  connected: () => !!socket && socket.readyState === WebSocket.OPEN && !!browserPlayer.playerId,
  /** Another tab or window of this browser is the player (this one waits). */
  elsewhere: false,
  /** Make it the selected player once it is connected (it was just turned on). */
  selectWhenConnected: false,
};

let socket = null;
let retry = 0;
let retryTimer = null;
/** The engine playing: {@link webAudio} or {@link htmlAudio}. */
let engine = null;

/** @param {FromRenderer} msg */
function report(msg) {
  if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(msg));
}

const absolute = (url) => serverRoot + url.replace(/^\/+/, '');

/** Browsers play only after a click or key on the page: ask for one, then call `retry`. */
let blocked = false;
function needGesture(retryFn) {
  if (!blocked) toast('Click anywhere on the page to let this browser play', 'error');
  blocked = true;
  const go = () => {
    window.removeEventListener('pointerdown', go, true);
    window.removeEventListener('keydown', go, true);
    blocked = false;
    retryFn();
  };
  window.addEventListener('pointerdown', go, true);
  window.addEventListener('keydown', go, true);
}

// ------------------------------------------------------------------ <audio> elements

const htmlAudio = (() => {
  // the playing element and the one holding the next track
  let current = null;
  let next = null;
  let startedSent = false;
  let lastProgress = 0;

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

  function init() {
    current ||= newAudio();
    next ||= newAudio();
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

  function play(a) {
    a.play().then(() => { blocked = false; }).catch((e) => {
      if (e.name === 'NotAllowedError') needGesture(() => { if (a === current) play(a); });
      else if (e.name !== 'AbortError') report({ type: 'error', message: e.message });
    });
  }

  function stop() {
    startedSent = false;
    for (const a of [current, next]) { if (a) { a.pause(); a.removeAttribute('src'); a.load(); } }
  }

  /** @param {ToRenderer} msg */
  function handle(msg) {
    init();
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
        stop();
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

  return {
    query: async () => ({ formats: playableFormats().join(',') }),
    handle,
    stop,
    // a click is what lets the page play later on
    unlock() { init(); current.play().catch(() => {}); },
  };
})();

// ------------------------------------------------------------------ Web Audio

const webAudio = (() => {
  const player = createWebAudioPlayer({
    report: (m) => report(m),
    absolute,
    // Opus decodes at 48 kHz; FLAC and raw samples are mostly 44.1 kHz
    rate: () => (browserPlayer.quality() === 'compact' ? 48000 : 44100),
    needGesture,
    blocked: () => blocked,
  });
  return {
    ...player,
    async query() {
      const q = await player.query(browserPlayer.quality() === 'compact');
      browserPlayer.decodes = q.decodes;
      return { formats: q.formats, rate: q.rate, clock: q.clock };
    },
  };
})();

/**
 * This browser's audio as it plays, for the visualiser: `{ctx, node}` while it plays
 * through Web Audio (keeping in sync), else null.
 */
export function browserAudioTap() {
  return engine === webAudio && browserPlayer.connected() ? webAudio.tap() : null;
}

// ------------------------------------------------------------------ the connection

/**
 * Connect as this browser's player.  `takeover`: take it from another page playing it (the
 * user asked, or this page was the player before a reload); otherwise, while another page
 * plays, this one waits (`elsewhere`) and looks again every little while.
 */
async function connect({ takeover = session.get() } = {}) {
  clearTimeout(retryTimer);
  if (!browserPlayer.enabled() || socket) return;
  engine = browserPlayer.precise() ? webAudio : htmlAudio;
  if (engine === webAudio) {
    try {
      await webAudio.prepare();
    } catch {
      engine = htmlAudio;
    }
    if (socket || !browserPlayer.enabled()) return;
  }
  const base = new URL(serverRoot, location.href);
  base.protocol = base.protocol === 'https:' ? 'wss:' : 'ws:';
  const query = await engine.query();
  if (socket || !browserPlayer.enabled()) return;
  const q = new URLSearchParams({ id: browserId(), name: browserPlayer.name(), ...query, ...(takeover ? { takeover: '1' } : {}) });
  const ws = new WebSocket(`${base.href}api/v1/browser-player?${q}`);
  const own = engine;
  socket = ws;
  ws.addEventListener('message', (e) => {
    let msg;
    try { msg = JSON.parse(e.data); } catch { return; }
    if (msg.type === 'busy' || msg.type === 'replaced') {
      // another page of this browser plays: wait, don't take it back
      if (msg.type === 'replaced') toast('This browser now plays in another tab or window');
      browserPlayer.elsewhere = true;
      session.set(false);
      return;
    }
    if (msg.type === 'welcome') {
      browserPlayer.playerId = msg.player;
      browserPlayer.elsewhere = false;
      session.set(true);
      retry = 0;
      // the clock the play points are on (before anything else)
      if (own === webAudio) report({ type: 'clock', jiffies: Math.round(performance.now()) >>> 0 });
      emit('browser-player', browserPlayer);
      return;
    }
    own.handle(msg);
  });
  ws.addEventListener('close', () => {
    if (socket !== ws) return;
    socket = null;
    browserPlayer.playerId = null;
    own.stop();
    emit('browser-player', browserPlayer);
    if (!browserPlayer.enabled()) return;
    if (browserPlayer.elsewhere) {
      // look now and then whether the other page has gone (without taking over)
      retryTimer = setTimeout(() => connect({ takeover: false }), ELSEWHERE_RETRY);
      return;
    }
    retry = Math.min(retry + 1, 6);
    retryTimer = setTimeout(connect, 500 * 2 ** retry);
  });
}

/** Play in this page rather than the other tab or window of this browser that plays. */
export function playHere() {
  if (!browserPlayer.enabled()) return;
  clearTimeout(retryTimer);
  (browserPlayer.precise() ? webAudio : htmlAudio).unlock();
  browserPlayer.selectWhenConnected = true;
  connect({ takeover: true });
}

function disconnect() {
  clearTimeout(retryTimer);
  const ws = socket;
  socket = null;
  browserPlayer.playerId = null;
  browserPlayer.elsewhere = false;
  session.set(false);
  ws?.close();
  engine?.stop();
  emit('browser-player', browserPlayer);
}

/**
 * Turn "play in this browser" on or off, or change how it plays (call from a click:
 * browsers only play after one).
 */
export function setBrowserPlayer(on, name, precise, quality) {
  if (name !== undefined) storage.set(NAME, name.trim());
  if (precise !== undefined) storage.set(PRECISE, precise ? '1' : '0');
  if (quality !== undefined && quality !== browserPlayer.quality()) {
    storage.set(QUALITY, quality);
    webAudio.reset();
  }
  storage.set(ENABLED, on ? '1' : '0');
  disconnect();
  if (on) {
    browserPlayer.selectWhenConnected = true;
    (browserPlayer.precise() ? webAudio : htmlAudio).unlock();
    // turned on (or changed) here: play here
    connect({ takeover: true });
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
