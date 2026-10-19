// RMS's Cast receiver app: everything a Chromecast plays for RMS plays here.
//
// Audio plays on Web Audio where the TV can decode it (WebCodecs): the same player as the
// web UI's (web-audio-player.js), driven over NS with the server's own renderer messages
// (RENDER: load, preload, pause, resume at a moment, hold, skip, stop), whose reports go
// back the same way (REPORT).  It starts at the exact moment, follows sync adjustments to
// the sample, and plays the next track without a gap.  Its clock is performance.now(),
// which every PONG carries, so RMS can line it up with its own.
//
// The server's visualiser video, and audio on TVs without WebCodecs, play through Cast's
// own media handling (LOAD) in the page's media element.  Either way it shows, as RMS
// says (customData.rms.visualizer, or the RENDER's `visual`):
// - `draw`: MilkDrop presets drawn to the audio with projectM (viz.js, shared with the
//   web UI);
// - `video`: the server's visualiser video;
// - `off`: the cover, blurred, behind the track.
// The playing track (viz.js's TrackOverlay) is over all three.  On a speaker (no screen)
// it only plays.
//
// On NS, RMS asks what it can do (HELLO), asks the time (PING, answered with PONG: this
// clock, and for the media element the position being heard, less what the audio takes
// to come out of the browser), tells it the next track of the visualiser video (TRACK),
// and hears how fast it draws (STATS) and what went wrong (ERROR), for its log, and whether
// it's on screen (VISIBLE, and in HELLO and PONG).
//
// Open it in a browser with ?dev&src=<audio url>[&title=…&artist=…&album=…&art=<url>]
// [&visualizer=draw|off] to try it without a Chromecast; add &engine to play it on the
// Web Audio player (as RMS's RENDER would), &debug for the frame rate.
import { Visualiser, TrackOverlay } from './viz.js';
import { createWebAudioPlayer } from './web-audio-player.js';
import { decodableFormats } from './decoders.js';

const NS = 'urn:x-cast:net.rms.visualizer';
const PROTOCOL = 3;
const SMALL_AFTER = 12000;
const params = new URLSearchParams(location.search);
const dev = params.has('dev');
const $ = (id) => document.getElementById(id);
const media = $('media');

let send = () => {};
function report(type, data, to) {
  try { send(Object.assign({ type }, data), to); } catch (e) { /* no sender connected */ }
}
const reported = new Set();
function fail(message) {
  console.warn('receiver:', message);
  if (reported.has(message)) return;
  reported.add(message);
  report('ERROR', { message });
}

// ------------------------------------------------------------------ drawing
const debugEl = $('debug');
debugEl.hidden = !params.has('debug');
let viz = null;
let overlay = null;
let mode = 'draw';
let started = false;
/** Cast's media player (the media element's), once started. */
let castPlayer = null;
/** A screen to show things on (not a speaker). */
let screen = true;

function startDrawing() {
  if (viz) return;
  viz = new Visualiser($('canvas'), {
    base: './',
    presetSeconds: 30,
    fps: 30,
    // as large as the TV keeps up with (up to 1080p and its screen), from 720p
    size: '',
    adaptive: true,
    level: 2,
    maxPixels: 1920 * 1080,
    onSize: (size) => { if (!debugEl.hidden) debugEl.textContent = `drawing at ${size}`; },
    // even the smallest is too much: RMS shows the video or the cover instead
    onTooSlow: (s) => { if (mode === 'draw') report('SLOW', s); },
    onStats: (s) => {
      if (mode !== 'draw') return;
      report('STATS', s);
      debugEl.textContent = `${s.fps} fps · ${s.size}\n${s.preset}`;
    },
    onPreset: (name) => { if (!debugEl.hidden) debugEl.textContent = name; },
    onError: fail,
  });
  viz.setPaused(mode !== 'draw');
  viz.start().catch((e) => fail(`the visualiser didn’t start: ${e.message || e}`));
}

function applySettings(s) {
  if (!s || typeof s !== 'object') return;
  if (s.version && s.version > PROTOCOL) {
    fail(`RMS speaks receiver protocol ${s.version}; this receiver knows ${PROTOCOL}`);
  }
  const o = {};
  if (Number(s.presetSeconds) > 0) o.presetSeconds = Number(s.presetSeconds);
  if (Number(s.fps) > 0) o.fps = Number(s.fps);
  // a size, or empty: as large as the TV keeps up with
  if (typeof s.renderSize === 'string') {
    o.size = s.renderSize;
    o.adaptive = s.renderSize === '';
  }
  viz?.setOptions(o);
}

/** `draw`, `video` or `off` (see the top). */
function setMode(m) {
  mode = ['draw', 'video', 'off'].includes(m) ? m : 'draw';
  document.body.classList.toggle('draw', mode === 'draw');
  document.body.classList.toggle('video', mode === 'video');
  document.body.classList.toggle('cover', mode === 'off');
  if (mode === 'draw' && screen) {
    startDrawing();
    if (!engineActive) hookAudio();
  }
  viz?.setPaused(mode !== 'draw');
  if (overlay) {
    // the now-playing screen keeps the track large
    overlay.smallAfter = mode === 'off' ? 0 : SMALL_AFTER;
    overlay.big(overlay.smallAfter);
  }
}

// ---------------------------------------------------------------- the audio
// The media element plays as usual until Web Audio is running; only then is it routed
// through the visualiser (once routed, it plays only through the audio context).
let actx = null;
let playingSince = 0;
let hooking = null;

/** Route the media element through Web Audio, once (calls while it's under way wait for it). */
function hookAudio() {
  hooking ||= hookOnce().finally(() => { hooking = null; });
  return hooking;
}

async function hookOnce() {
  if (!started) return;
  if (actx) {
    if (actx.state !== 'running') await actx.resume().catch(() => {});
    return;
  }
  const ctx = new (window.AudioContext || window.webkitAudioContext)();
  await ctx.resume().catch(() => {});
  if (ctx.state !== 'running') {
    ctx.close();
    fail('Web Audio wouldn’t start, so the visuals can’t hear the music');
    return;
  }
  actx = ctx;
  const src = actx.createMediaElementSource(media);
  src.connect(actx.destination);
  viz?.setAudio(actx, src);
}

// all silence for a while: the browser won't let the page read the stream
setInterval(() => {
  if (mode === 'draw' && actx && viz && playingSince && !viz.heardAt && !media.paused
      && media.volume > 0 && performance.now() - playingSince > 8000) {
    fail('the music reads as silence: the browser may be blocking access to the stream (CORS)');
  }
}, 2000);

/** Where playback is as it's heard: the media's position, less the audio still on its way out. */
function heardPosition() {
  const latency = actx ? (actx.outputLatency || 0) + (actx.baseLatency || 0) : 0;
  return Math.max(0, media.currentTime - latency * media.playbackRate);
}

// ------------------------------------------------------------ Web Audio
// The tracks RENDER loads and preloads, in order, and which of them plays.
let engineTracks = [];
let engineIndex = -1;
let engineActive = false;
/** The engine's last word on the position: seconds into the track at performance.now(). */
let engineAt = null;
let enginePlaying = false;

const engine = createWebAudioPlayer({
  report: (m) => {
    onEngineReport(m);
    report('REPORT', { msg: m });
  },
  // (RMS sends the server's own URLs)
  absolute: (url) => url,
  rate: () => 44100,
  // a receiver plays without a click; should it be refused, try again soon
  needGesture: (retry) => setTimeout(retry, 1000),
  blocked: () => false,
});

function onEngineReport(m) {
  switch (m.type) {
    case 'started':
      engineIndex += 1;
      if (engineTracks[engineIndex]) showTrack(engineTracks[engineIndex]);
      enginePlaying = true;
      engineAt = { elapsed: 0, at: performance.now() };
      setState('playing');
      break;
    case 'progress':
      engineAt = { elapsed: m.elapsed, at: performance.now() };
      break;
    case 'paused':
      enginePlaying = false;
      setState('paused');
      break;
    case 'resumed':
      enginePlaying = true;
      setState('playing');
      break;
    case 'ended':
      enginePlaying = false;
      setState('idle');
      break;
    default:
      break;
  }
}

function engineElapsed() {
  if (!engineAt) return 0;
  return engineAt.elapsed + (enginePlaying ? (performance.now() - engineAt.at) / 1000 : 0);
}

/** A renderer message for the Web Audio player, with what goes with a track. */
function onRender(data) {
  const cmd = data.cmd || {};
  if (cmd.type === 'load' || cmd.type === 'preload') {
    if (cmd.type === 'load') {
      engineTracks = [];
      engineIndex = -1;
      engineAt = null;
      enginePlaying = false;
      if (!engineActive) {
        // from the media element to Web Audio
        engineActive = true;
        castPlayer?.stop();
      }
      setMode(data.visual);
      applySettings(data.rms);
    }
    engineTracks.push(data.track || {});
  }
  if (cmd.type === 'stop') {
    engineTracks = [];
    engineIndex = -1;
    enginePlaying = false;
    setState('idle');
  }
  engine.handle(cmd);
}

// the visualiser hears the Web Audio player's output
setInterval(() => {
  if (!engineActive || mode !== 'draw' || !viz) return;
  const tap = engine.tap();
  if (tap) viz.setAudio(tap.ctx, tap.node);
}, 500);

// -------------------------------------------------------------- track info
let duration = 0;
// the length RMS gave with the media handling's LOAD
let loadedDuration = 0;

function showTrack(t) {
  if (!overlay) return;
  overlay.track(t);
  if (t.art) $('backdrop').style.backgroundImage = `url("${t.art.replace(/"/g, '%22')}")`;
  duration = known(t.duration);
}

/** A length, or 0 when it isn't known (a stream's is Infinity). */
function known(d) {
  const n = Number(d);
  return Number.isFinite(n) && n > 0 ? n : 0;
}

function fromMetadata(meta, length) {
  meta = meta || {};
  return {
    title: meta.title || '',
    artist: meta.artist || meta.subtitle || meta.albumArtist || '',
    album: meta.albumName || '',
    art: (meta.images && meta.images[0] && meta.images[0].url) || '',
    duration: length,
  };
}

function setState(s) {
  overlay?.state(s);
  document.body.classList.toggle('active', s !== 'idle');
}

setInterval(() => {
  if (!overlay) return;
  if (engineActive) {
    overlay.progress(engineElapsed(), duration);
    return;
  }
  // (the video's own length is the whole stream; RMS says each track's)
  const d = mode !== 'video' && isFinite(media.duration) && media.duration > 0 ? media.duration : duration;
  overlay.progress(mode === 'video' ? trackElapsed() : media.currentTime || 0, d);
}, 250);

// in the visualiser video, where the current track began in it
let trackStart = 0;
function trackElapsed() {
  return Math.max(0, (media.currentTime || 0) - trackStart);
}

media.addEventListener('playing', () => {
  if (engineActive) return;
  setState('playing');
  playingSince = playingSince || performance.now();
  if (mode === 'draw' && screen) hookAudio();
});
media.addEventListener('pause', () => { if (!media.ended && !engineActive) setState('paused'); });
media.addEventListener('emptied', () => { playingSince = 0; trackStart = 0; });

/** Whether this device draws with WebGL 2 (projectM needs it). */
function hasWebGL2() {
  try {
    return !!document.createElement('canvas').getContext('webgl2');
  } catch (e) {
    return false;
  }
}

// Whether the page is on screen: Google TV keeps a cast app playing behind its home screen
// after Home is pressed.  RMS brings it back to the front when it starts playing.
let visible = !document.hidden;
function noteVisible(v) {
  if (v === visible) return;
  visible = v;
  report('VISIBLE', { visible });
}
document.addEventListener('visibilitychange', () => noteVisible(!document.hidden));

let canEngine = null;
decodableFormats().then((d) => { canEngine = d.includes('flc') ? d : []; }).catch(() => { canEngine = []; });

function onMessage(data, from) {
  if (!data || typeof data !== 'object') return;
  if (data.type === 'PING') {
    // (this clock first: the answer is taken as true halfway through the round trip)
    const t = performance.now();
    report('PONG', {
      id: data.id,
      t,
      position: heardPosition(),
      rate: media.playbackRate,
      playing: !media.paused && !media.ended,
      visible,
    }, from);
  } else if (data.type === 'HELLO') {
    const answer = () => report('HELLO', {
      version: PROTOCOL,
      engine: (canEngine || []).length > 0 && typeof AudioContext !== 'undefined',
      screen,
      webgl2: hasWebGL2(),
      size: `${Math.round(window.screen.width * (window.devicePixelRatio || 1))}x${Math.round(window.screen.height * (window.devicePixelRatio || 1))}`,
      decodes: canEngine || [],
      visible,
    }, from);
    if (canEngine) answer();
    else setTimeout(answer, 300);
  } else if (data.type === 'RENDER') {
    onRender(data);
  } else if (data.type === 'TRACK') {
    trackStart = Number(data.start) || 0;
    showTrack(data);
  }
}

// ------------------------------------------------------------------ start
function startCast() {
  const framework = cast.framework;
  const context = framework.CastReceiverContext.getInstance();
  const player = context.getPlayerManager();
  player.setMediaElement(media);
  // RMS keeps the TV in step with its sync group by playing a little faster or slower
  const C = framework.messages.Command;
  player.setSupportedMediaCommands(C.ALL_BASIC_MEDIA | (C.PLAYBACK_RATE || 8192), true);
  castPlayer = player;
  player.setMessageInterceptor(framework.messages.MessageType.LOAD, (req) => {
    if (engineActive) {
      // from Web Audio to the media element
      engineActive = false;
      engine.handle({ type: 'stop' });
    }
    // the track's length from the library: Cast's own for a stream is Infinity
    loadedDuration = known(req.media?.duration);
    const rms = (req.customData && req.customData.rms) || {};
    const video = String(req.media?.contentType || '').startsWith('video/');
    setMode(video ? 'video' : rms.visualizer);
    applySettings(rms);
    return req;
  });
  player.addEventListener(framework.events.EventType.PLAYER_LOAD_COMPLETE, () => {
    const info = player.getMediaInformation() || {};
    trackStart = 0;
    showTrack(fromMetadata(info.metadata, known(info.duration) || loadedDuration));
  });
  const options = new framework.CastReceiverOptions();
  options.customNamespaces = { [NS]: framework.system.MessageType.JSON };
  // plain streams: no adaptive-streaming player needed
  options.skipPlayersLoad = true;
  options.statusText = 'RMS';
  // playing on Web Audio isn't Cast media: stay up while it plays
  options.disableIdleTimeout = true;
  context.addCustomMessageListener(NS, (e) => onMessage(e.data, e.senderId));
  context.addEventListener(framework.system.EventType.VISIBILITY_CHANGED, (e) => noteVisible(e.isVisible !== false));
  context.start(options);
  send = (data, to) => context.sendCustomMessage(NS, to, data);
  started = true;
  // a speaker: nothing to show
  const caps = context.getDeviceCapabilities?.();
  screen = !caps || caps[framework.system.DeviceCapabilities.DISPLAY_SUPPORTED] !== false;
  if (screen) {
    overlay = new TrackOverlay($('stage'), { smallAfter: SMALL_AFTER });
    $('idle-hint').textContent = 'Waiting for music';
    startDrawing();
  }
}

function startDev() {
  started = true;
  overlay = new TrackOverlay($('stage'), { smallAfter: SMALL_AFTER });
  startDrawing();
  setMode(params.get('visualizer') || 'draw');
  const src = params.get('src');
  $('idle-hint').textContent = src ? 'Loading…' : 'Add ?dev&src=<audio url> to play something';
  if (!src) return;
  if (params.has('engine')) {
    onRender({
      cmd: { type: 'load', url: new URL(src, location.href).href, format: 'flc', autostart: true },
      visual: params.get('visualizer') || 'draw',
      track: { title: params.get('title') || '', artist: params.get('artist') || '', album: params.get('album') || '', art: params.get('art') || '' },
    });
    window.rmsEngine = engine;
    return;
  }
  media.src = src;
  media.loop = true;
  showTrack({
    title: params.get('title') || src.split('/').pop(),
    artist: params.get('artist') || '',
    album: params.get('album') || '',
    art: params.get('art') || '',
  });
  media.play().catch(() => {
    $('idle-hint').textContent = 'Click to start';
    window.addEventListener('click', () => media.play(), { once: true });
  });
}

if (dev) startDev();
else if (window.cast && cast.framework) startCast();
else $('idle-hint').textContent = 'The Cast receiver framework didn’t load';
