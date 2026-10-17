// RMS's Cast receiver app: plays the track RMS loads (an ordinary audio stream from the
// server), draws MilkDrop presets to it with projectM, and shows the track's cover and
// title.  The drawing and the track info are viz.js, shared with the web UI.
//
// RMS sends its visualiser settings with each LOAD (customData.rms) and hears back on
// NS: how fast the TV draws (STATS), and what went wrong (ERROR), for its log.
//
// Open it in a browser with ?dev&src=<audio url>[&title=…&artist=…&album=…&art=<url>]
// to try it without a Chromecast; add &debug for the frame rate on screen.
import { Visualiser, TrackOverlay } from './viz.js';

const NS = 'urn:x-cast:net.rms.visualizer';
const PROTOCOL = 1;
const params = new URLSearchParams(location.search);
const dev = params.has('dev');
const $ = (id) => document.getElementById(id);
const media = $('media');

let send = () => {};
function report(type, data) {
  try { send(Object.assign({ type }, data)); } catch (e) { /* no sender connected */ }
}
const reported = new Set();
function fail(message) {
  console.warn('visualiser:', message);
  if (reported.has(message)) return;
  reported.add(message);
  report('ERROR', { message });
}

// ------------------------------------------------------------------ drawing
const debugEl = $('debug');
debugEl.hidden = !params.has('debug');
const viz = new Visualiser($('canvas'), {
  base: './',
  presetSeconds: 30,
  fps: 30,
  size: '1280x720',
  onStats: (s) => {
    report('STATS', s);
    debugEl.textContent = `${s.fps} fps · ${s.size}\n${s.preset}`;
  },
  onPreset: (name) => { if (!debugEl.hidden) debugEl.textContent = name; },
  onError: fail,
});

function applySettings(s) {
  if (!s || typeof s !== 'object') return;
  if (s.version && s.version > PROTOCOL) {
    fail(`RMS speaks receiver protocol ${s.version}; this receiver knows ${PROTOCOL}`);
  }
  const o = {};
  if (Number(s.presetSeconds) > 0) o.presetSeconds = Number(s.presetSeconds);
  if (Number(s.fps) > 0) o.fps = Number(s.fps);
  if (typeof s.renderSize === 'string') o.size = s.renderSize;
  viz.setOptions(o);
}

// ---------------------------------------------------------------- the audio
// The media element plays as usual until Web Audio is running; only then is it routed
// through the visualiser (once routed, it plays only through the audio context).
let actx = null;
let playingSince = 0;

async function hookAudio() {
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
  viz.setAudio(actx, src);
}

// all silence for a while: the browser won't let the page read the stream
setInterval(() => {
  if (actx && playingSince && !viz.heardAt && !media.paused && media.volume > 0
      && performance.now() - playingSince > 8000) {
    fail('the music reads as silence: the browser may be blocking access to the stream (CORS)');
  }
}, 2000);

// -------------------------------------------------------------- track info
const overlay = new TrackOverlay($('stage'));
let duration = 0;

function showTrack(meta, length) {
  meta = meta || {};
  overlay.track({
    title: meta.title || '',
    artist: meta.artist || meta.subtitle || meta.albumArtist || '',
    album: meta.albumName || '',
    art: (meta.images && meta.images[0] && meta.images[0].url) || '',
  });
  duration = Number(length) || 0;
}

function setState(s) {
  overlay.state(s);
  document.body.classList.toggle('active', s !== 'idle');
}

setInterval(() => {
  const d = (isFinite(media.duration) && media.duration > 0) ? media.duration : duration;
  overlay.progress(media.currentTime || 0, d);
}, 250);

media.addEventListener('playing', () => {
  setState('playing');
  playingSince = playingSince || performance.now();
  hookAudio();
});
media.addEventListener('pause', () => { if (!media.ended) setState('paused'); });
media.addEventListener('emptied', () => { playingSince = 0; });

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
    applySettings(req.customData && req.customData.rms);
    return req;
  });
  player.addEventListener(framework.events.EventType.PLAYER_LOAD_COMPLETE, () => {
    const info = player.getMediaInformation() || {};
    showTrack(info.metadata, info.duration);
  });
  const options = new framework.CastReceiverOptions();
  options.customNamespaces = { [NS]: framework.system.MessageType.JSON };
  // plain audio streams: no adaptive-streaming player needed
  options.skipPlayersLoad = true;
  options.statusText = 'RMS visualiser';
  context.start(options);
  send = (data) => context.sendCustomMessage(NS, undefined, data);
  $('idle-hint').textContent = 'Waiting for music';
}

function startDev() {
  const src = params.get('src');
  $('idle-hint').textContent = src ? 'Loading…' : 'Add ?dev&src=<audio url> to play something';
  if (!src) return;
  media.src = src;
  media.loop = true;
  showTrack({
    title: params.get('title') || src.split('/').pop(),
    artist: params.get('artist') || '',
    albumName: params.get('album') || '',
    images: params.get('art') ? [{ url: params.get('art') }] : [],
  });
  media.play().catch(() => {
    $('idle-hint').textContent = 'Click to start';
    window.addEventListener('click', () => media.play(), { once: true });
  });
}

if (dev) startDev();
else if (window.cast && cast.framework) startCast();
else $('idle-hint').textContent = 'The Cast receiver framework didn’t load';

viz.start().catch((e) => fail(`the visualiser didn’t start: ${e.message || e}`));
