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
import { PcmOutput } from './pcm-output.js';
import { pcmDecoder, codecDecoder, decodableFormats } from './decoders.js';

/** @typedef {import('./api-types').ToRenderer} ToRenderer */
/** @typedef {import('./api-types').FromRenderer} FromRenderer */

const ENABLED = 'rms.browserPlayer';
const PRECISE = 'rms.browserPrecise';
const QUALITY = 'rms.browserQuality';
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
  /** Start playing once this much is buffered (seconds); report ready at this much. */
  const START_AFTER = 0.5;
  const READY_AFTER = 1.0;
  /** Fetch no further ahead than this (seconds): a track is not held in memory whole. */
  const AHEAD = 30;

  /** @type {AudioContext|null} */
  let ctx = null;
  /** Sends a message to the output (pcm-output.js). */
  let post = () => {};
  /** @type {GainNode|null} */
  let gain = null;
  /** The output node (worklet or script processor), before the volume. */
  let out = null;
  let loading = null;
  let sr = 44100;
  let volume = 1;

  let seq = 0;
  /** The track playing, or loaded and waiting to start: {id, autostart, started, ready, fetched}. */
  let current = null;
  /** The track after it, preloaded. */
  let next = null;
  /** Aborts the fetches of what is loaded. */
  let abort = new AbortController();
  /** Fetches run one after another, in queue order. */
  let fetching = Promise.resolve();
  let fetchesRunning = 0;
  let paused = false;
  /** The last word from the worklet on where output is. */
  let pos = null;
  /** Frames sent to the worklet and not yet played, as far as known here. */
  let buffered = 0;
  let lastProgress = 0;

  /** Create the audio context and the worklet (once). */
  function setup() {
    if (loading) return loading;
    // Opus decodes at 48 kHz; FLAC and raw samples are mostly 44.1 kHz
    const rate = browserPlayer.quality() === 'compact' ? 48000 : 44100;
    ctx = new AudioContext({ sampleRate: rate, latencyHint: 'playback' });
    sr = ctx.sampleRate;
    gain = ctx.createGain();
    gain.gain.value = volume;
    gain.connect(ctx.destination);
    if (ctx.audioWorklet && typeof AudioWorkletNode !== 'undefined') {
      loading = ctx.audioWorklet.addModule(new URL('./pcm-worklet.js', import.meta.url)).then(() => {
        const node = new AudioWorkletNode(ctx, 'rms-pcm', { numberOfInputs: 0, outputChannelCount: [2] });
        node.connect(gain);
        out = node;
        node.port.onmessage = (e) => fromOutput(e.data);
        post = (m, transfer) => node.port.postMessage(m, transfer || []);
      });
    } else {
      // no AudioWorklet (not https): the same output on the page's own thread
      const pcm = new PcmOutput(fromOutput, 1);
      const node = ctx.createScriptProcessor(2048, 1, 2);
      node.onaudioprocess = (e) => {
        const b = e.outputBuffer;
        pcm.render(b.getChannelData(0), b.getChannelData(1), Math.round(e.playbackTime * sr));
      };
      node.connect(gain);
      out = node;
      post = (m) => pcm.message(m);
      loading = Promise.resolve();
    }
    return loading;
  }

  /** The audio context must run to play: browsers allow that after a click or key. */
  function running() {
    if (!ctx || ctx.state === 'running') return;
    ctx.resume().then(() => {
      if (ctx.state !== 'running') needGesture(running);
    }).catch(() => needGesture(running));
    // a context that stays suspended needs a click
    setTimeout(() => { if (ctx.state === 'suspended' && !blocked) needGesture(running); }, 500);
  }

  /**
   * A moment of the audio clock and when it is heard: `contextTime` seconds of the context
   * are heard at `performanceTime` (performance.now() time).
   */
  function timestamp() {
    const ts = ctx.getOutputTimestamp?.();
    if (ts?.performanceTime) return ts;
    // estimated: what is rendered now is heard after the output's latency
    return {
      contextTime: ctx.currentTime,
      performanceTime: performance.now() + ((ctx.outputLatency || 0) + (ctx.baseLatency || 0)) * 1000,
    };
  }

  /** The context frame heard at `ms` (performance.now() time). */
  function frameAt(ms) {
    const ts = timestamp();
    return Math.round((ts.contextTime + (ms - ts.performanceTime) / 1000) * sr);
  }

  /** When context frame `frame` is heard (performance.now() time). */
  function heardAt(frame) {
    const ts = timestamp();
    return ts.performanceTime + (frame / sr - ts.contextTime) * 1000;
  }

  /** Seconds into the current track heard at `now`. */
  function elapsed(now) {
    if (!pos || !current || pos.track !== current.id) return 0;
    let e = pos.played / sr;
    if (pos.playing) e += (now - heardAt(pos.frame)) / 1000;
    return Math.max(0, e);
  }

  function progress(force) {
    const now = performance.now();
    if (!current?.started || (!force && now - lastProgress < 1000)) return;
    lastProgress = now;
    report({ type: 'progress', elapsed: elapsed(now), jiffies: Math.round(now) >>> 0 });
  }

  function fromOutput(m) {
    switch (m.type) {
      case 'pos':
        pos = m;
        buffered = m.buffered;
        if (m.playing && !paused) progress(false);
        break;
      case 'track':
        if (next && m.track === next.id) {
          current = next;
          next = null;
        }
        if (current && m.track === current.id && !current.started) {
          current.started = true;
          pos = null;
          report({ type: 'started' });
          if (current.fetched) report({ type: 'fetched' });
        }
        break;
      case 'drained':
        // the end, unless more is on its way (then it is only an underrun)
        if (current?.started && !paused && fetchesRunning === 0) {
          progress(true);
          report({ type: 'ended' });
          current = null;
        }
        break;
      default:
        break;
    }
  }

  /** Start the current track now, or at `at` (milliseconds of performance.now()). */
  function start(at) {
    running();
    paused = false;
    post({ type: 'start', frame: at === undefined ? 0 : frameAt(at) });
  }

  const sleep = (ms) => new Promise((r) => { setTimeout(r, ms); });

  /** Fetch track `t`'s audio, decode it and send the samples to the worklet. */
  async function fetchTrack(t, url, format, pcm, signal) {
    let res;
    try {
      res = await fetch(absolute(url), { signal });
      if (!res.ok || !res.body) throw new Error(`HTTP ${res.status}`);
    } catch (e) {
      if (!signal.aborted) report({ type: 'error', message: e.message });
      return;
    }
    let total = 0;
    const deliver = (left, right) => {
      if (signal.aborted) return;
      // (counted first: posting hands the buffers over, and leaves them empty here)
      total += left.length;
      buffered += left.length;
      post({ type: 'audio', track: t.id, left, right }, [left.buffer, right.buffer]);
      // enough to begin
      if (t === current && !t.started && !t.ready) {
        if (t.autostart && total >= START_AFTER * sr) { t.ready = true; start(); }
        if (!t.autostart && total >= READY_AFTER * sr) { t.ready = true; report({ type: 'ready' }); }
      }
    };
    const fail = (e) => { if (!signal.aborted) report({ type: 'error', message: e.message || String(e) }); };
    const decoder = format === 'flc' || format === 'ops'
      ? codecDecoder(format, sr, deliver, fail)
      : pcmDecoder(pcm || { bits: 16, rate: 44100, channels: 2, little_endian: true }, sr, deliver);
    const reader = res.body.getReader();
    try {
      for (;;) {
        // keep only so much ahead of what plays
        while (buffered > AHEAD * sr && !signal.aborted) await sleep(250);
        const { done, value } = await reader.read();
        if (signal.aborted) return;
        if (done) break;
        await decoder.push(value);
      }
      await decoder.end();
    } catch (e) {
      fail(e);
      return;
    }
    if (signal.aborted) return;
    if (t === current && !t.started && !t.ready) {
      // a short track: all there is
      t.ready = true;
      if (t.autostart) start();
      else report({ type: 'ready' });
    }
    // the next track can come (once this one plays: until then, it is the next one)
    t.fetched = true;
    if (t.started) report({ type: 'fetched' });
  }

  function queueFetch(t, url, format, pcm) {
    const signal = abort.signal;
    fetching = fetching.then(async () => {
      if (signal.aborted) return;
      fetchesRunning++;
      try { await fetchTrack(t, url, format, pcm, signal); } finally { fetchesRunning--; }
    });
  }

  function stop() {
    abort.abort();
    abort = new AbortController();
    fetching = Promise.resolve();
    current = null;
    next = null;
    paused = false;
    pos = null;
    buffered = 0;
    post({ type: 'clear' });
  }

  /** @param {ToRenderer} msg */
  async function handle(msg) {
    await setup();
    switch (msg.type) {
      case 'load':
        stop();
        current = { id: ++seq, autostart: !!msg.autostart, started: false, ready: false, fetched: false };
        if (msg.autostart) running();
        queueFetch(current, msg.url, msg.format, msg.pcm);
        break;
      case 'preload':
        next = { id: ++seq, autostart: true, started: false, ready: true, fetched: false };
        queueFetch(next, msg.url, msg.format, msg.pcm);
        break;
      case 'pause':
        if (!current) break;
        progress(true);
        paused = true;
        post({ type: 'pause' });
        if (current.started) report({ type: 'paused' });
        break;
      case 'resume':
        if (!current) break;
        start(msg.at ?? undefined);
        if (current.started) report({ type: 'resumed' });
        break;
      case 'stop':
        stop();
        break;
      case 'hold':
        post({ type: 'hold', frames: Math.round((msg.ms * sr) / 1000) });
        break;
      case 'skip':
        post({ type: 'skip', frames: Math.round((msg.ms * sr) / 1000) });
        break;
      case 'volume': {
        const v = Math.max(0, Math.min(100, msg.volume)) / 100;
        volume = msg.muted ? 0 : v * v;
        gain?.gain.setTargetAtTime(volume, ctx.currentTime, 0.02);
        break;
      }
      default:
        break;
    }
  }

  return {
    /** The audio as it plays, before the volume (for the visualiser). */
    tap: () => (ctx && out ? { ctx, node: out } : null),
    async query() {
      browserPlayer.decodes = await decodableFormats();
      const has = (f) => browserPlayer.decodes.includes(f);
      // Opus first asks the server for Opus (unless a synced player can't take it)
      const formats = browserPlayer.quality() === 'compact' && has('ops') ? ['ops', 'flc', 'pcm']
        : has('flc') ? ['flc', 'pcm'] : ['pcm'];
      return { formats: formats.join(','), rate: String(sr), clock: '1' };
    },
    /** Messages are handled in order, though setting up is asynchronous. */
    handle: (() => {
      let chain = Promise.resolve();
      return (msg) => { chain = chain.then(() => handle(msg)).catch((e) => report({ type: 'error', message: e.message })); };
    })(),
    stop,
    unlock() { setup().then(running).catch(() => {}); },
    /** Start again with a new audio context (its rate follows the quality). */
    reset() {
      stop();
      ctx?.close().catch(() => {});
      ctx = null;
      out = null;
      loading = null;
      post = () => {};
    },
    prepare: setup,
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

async function connect() {
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
  const q = new URLSearchParams({ id: browserId(), name: browserPlayer.name(), ...query });
  const ws = new WebSocket(`${base.href}api/v1/browser-player?${q}`);
  const own = engine;
  socket = ws;
  ws.addEventListener('message', (e) => {
    let msg;
    try { msg = JSON.parse(e.data); } catch { return; }
    if (msg.type === 'welcome') {
      browserPlayer.playerId = msg.player;
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
    connect();
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
