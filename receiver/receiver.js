// RMS's Cast receiver app: everything a Chromecast plays for RMS plays here.  It plays
// what RMS loads (an audio stream from the server, or the server's visualiser video) in
// its own media element, and shows, as RMS says with each LOAD (customData.rms.visualizer):
// - `draw`: MilkDrop presets drawn to the audio with projectM (viz.js, shared with the
//   web UI);
// - `video`: the server's visualiser video;
// - `off`: the cover, blurred, behind the track.
// The playing track (viz.js's TrackOverlay) is over all three.  On a speaker (no screen)
// it only plays.
//
// On NS, RMS asks where playback is (PING, answered with PONG: the position being heard,
// less what the audio takes to come out of the browser), tells it the next track of the
// visualiser video (TRACK), and hears how fast it draws (STATS) and what went wrong
// (ERROR), for its log.
//
// Open it in a browser with ?dev&src=<audio url>[&title=…&artist=…&album=…&art=<url>]
// [&visualizer=draw|off] to try it without a Chromecast; add &debug for the frame rate.
import { Visualiser, TrackOverlay } from './viz.js';

const NS = 'urn:x-cast:net.rms.visualizer';
const PROTOCOL = 2;
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
/** A screen to show things on (not a speaker). */
let screen = true;

function startDrawing() {
  if (viz) return;
  viz = new Visualiser($('canvas'), {
    base: './',
    presetSeconds: 30,
    fps: 30,
    size: '1280x720',
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
  if (typeof s.renderSize === 'string') o.size = s.renderSize;
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
    hookAudio();
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

async function hookAudio() {
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

// -------------------------------------------------------------- track info
let duration = 0;

function showTrack(t) {
  if (!overlay) return;
  overlay.track(t);
  if (t.art) $('backdrop').style.backgroundImage = `url("${t.art.replace(/"/g, '%22')}")`;
  duration = Number(t.duration) || 0;
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
  setState('playing');
  playingSince = playingSince || performance.now();
  if (mode === 'draw' && screen) hookAudio();
});
media.addEventListener('pause', () => { if (!media.ended) setState('paused'); });
media.addEventListener('emptied', () => { playingSince = 0; trackStart = 0; });

function onMessage(data, from) {
  if (!data || typeof data !== 'object') return;
  if (data.type === 'PING') {
    report('PONG', {
      id: data.id,
      position: heardPosition(),
      rate: media.playbackRate,
      playing: !media.paused && !media.ended,
    }, from);
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
  player.setMessageInterceptor(framework.messages.MessageType.LOAD, (req) => {
    const rms = (req.customData && req.customData.rms) || {};
    const video = String(req.media?.contentType || '').startsWith('video/');
    setMode(video ? 'video' : rms.visualizer);
    applySettings(rms);
    return req;
  });
  player.addEventListener(framework.events.EventType.PLAYER_LOAD_COMPLETE, () => {
    const info = player.getMediaInformation() || {};
    trackStart = 0;
    showTrack(fromMetadata(info.metadata, info.duration));
  });
  const options = new framework.CastReceiverOptions();
  options.customNamespaces = { [NS]: framework.system.MessageType.JSON };
  // plain streams: no adaptive-streaming player needed
  options.skipPlayersLoad = true;
  options.statusText = 'RMS';
  context.addCustomMessageListener(NS, (e) => onMessage(e.data, e.senderId));
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
