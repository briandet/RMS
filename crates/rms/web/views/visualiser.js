// The visualiser: MilkDrop presets drawn by projectM in this browser (viz.js, shared with
// RMS's Cast receiver) to the music this browser plays, over the whole window, with the
// playing track in the corner.
//
// It hears only what this browser plays through Web Audio, so it's offered while this
// browser is a player in the selected player's group (or is the selected player), playing
// in sync.  projectM and the presets come from the receiver's site (viz.js ASSETS), or from
// `rms.vizAssets` in local storage (a URL ending in /, e.g. a local build of receiver/).

import { on, currentPlayer, currentTrack, elapsed } from '../store.js';
import { sizedArtwork, currentArtUrl, getSettings } from '../api.js';
import { browserPlayer, browserAudioTap } from '../browser-player.js';
import { Visualiser, TrackOverlay, ASSETS } from '../viz.js';
import { h, icon, storage, toast } from '../util.js';

const DETAIL = 'rms.vizDetail';

/** Whether the visualiser can hear player `p`: this browser plays it, or is synced with it. */
export function visualiserFor(p) {
  const me = browserPlayer.playerId;
  if (!p || !me || !browserPlayer.connected()) return false;
  return p.id === me || (p.synced_with || []).includes(me);
}

let open = null;

/** Open the visualiser over the page (once). */
export function openVisualiser() {
  if (open) return;
  if (!visualiserFor(currentPlayer())) {
    toast('The visualiser plays along with this browser: turn on “Play in this browser” and play here or sync with this player', 'error');
    return;
  }
  if (!browserPlayer.precise()) {
    toast('The visualiser needs “Keep in sync with other players” (Settings → This browser)', 'error');
    return;
  }
  open = build();
}

function build() {
  const canvas = h('canvas', { class: 'viz-canvas', id: 'rms-viz-canvas' });
  const hint = h('div', { class: 'viz-hint', role: 'status' }, 'Starting the visualiser…');
  const preset = h('span', { class: 'viz-preset' });
  const btn = (name, label, onclick, text) =>
    h('button', { class: 'viz-btn', type: 'button', title: label, 'aria-label': label, onclick },
      name ? icon(name) : null, text ? h('span', {}, text) : null);
  // automatic: as large as this browser keeps up with; or full or half resolution
  const DETAILS = { auto: 'Auto detail', full: 'Full detail', half: 'Half detail' };
  const detailOptions = (d) => ({ adaptive: d === 'auto', scale: d === 'half' ? 0.5 : 1 });
  let detail = storage.get(DETAIL);
  if (!(detail in DETAILS)) detail = 'auto';
  const detailBtn = btn(null, 'How much detail to draw: automatic (as much as keeps up), full or half', () => {
    detail = detail === 'auto' ? 'full' : detail === 'full' ? 'half' : 'auto';
    storage.set(DETAIL, detail);
    detailBtn.querySelector('span').textContent = DETAILS[detail];
    viz.setOptions(detailOptions(detail));
  }, DETAILS[detail]);
  const bar = h('div', { class: 'viz-bar' },
    preset,
    btn('next', 'Next preset (N)', () => viz.next(false), 'Next preset'),
    detailBtn,
    btn('columns', 'Full screen (F)', fullscreen, 'Full screen'),
    btn('close', 'Close (Esc)', close));
  const layer = h('div', { class: 'viz-layer', role: 'dialog', 'aria-label': 'Visualiser' }, canvas, hint, bar);
  document.body.append(layer);
  document.body.classList.add('viz-open');

  const overlay = new TrackOverlay(layer);
  const viz = new Visualiser(canvas, {
    base: storage.get('rms.vizAssets') || ASSETS,
    ...detailOptions(detail),
    onPreset: (name) => { preset.textContent = name; },
    onError: (m) => console.warn('visualiser:', m),
  });
  viz.start().then(() => update()).catch((e) => {
    hint.textContent = `The visualiser didn’t start: ${e.message || e}`;
    hint.hidden = false;
  });
  getSettings().then((s) => {
    const secs = s?.visualizer_preset_seconds?.value;
    if (secs) viz.setOptions({ presetSeconds: secs });
  }).catch(() => {});

  // what's playing, and this browser's audio (which comes and goes with the player)
  function update() {
    const p = currentPlayer();
    const tr = currentTrack();
    const tap = visualiserFor(p) ? browserAudioTap() : null;
    if (tap) viz.setAudio(tap.ctx, tap.node);
    else viz.clearAudio();
    if (viz.ready) {
      hint.hidden = !!tap;
      hint.textContent = 'Waiting for this browser to play';
    }
    const playing = p && tr && (p.mode === 'play' || p.mode === 'pause');
    overlay.state(!playing ? 'idle' : p.mode === 'play' ? 'playing' : 'paused');
    if (!playing) return;
    const art = tr.artwork ? sizedArtwork(tr.artwork, 600) : currentArtUrl(p.id, `${tr.id ?? ''}-${tr.title || ''}`, 600);
    overlay.track({ title: tr.title, artist: tr.artist, album: tr.album || tr.station, art });
    overlay.progress(elapsed(), tr.duration || p.duration || 0);
  }
  const offs = [on('status', update), on('player', update), on('players', update), on('browser-player', update)];
  const ticker = setInterval(update, 500);

  // the controls fade while the pointer rests
  let idle = 0;
  const wake = () => {
    layer.classList.remove('idle');
    clearTimeout(idle);
    idle = setTimeout(() => layer.classList.add('idle'), 3000);
  };
  wake();
  layer.addEventListener('pointermove', wake);
  layer.addEventListener('pointerdown', wake);

  function fullscreen() {
    if (document.fullscreenElement) document.exitFullscreen?.().catch(() => {});
    else layer.requestFullscreen?.().catch(() => toast('Full screen isn’t available here', 'error'));
  }
  const keys = (e) => {
    if (e.key === 'Escape' && !document.fullscreenElement) close();
    else if (e.key === 'n' || e.key === 'N') viz.next(false);
    else if (e.key === 'f' || e.key === 'F') fullscreen();
    else return;
    e.preventDefault();
    wake();
  };
  window.addEventListener('keydown', keys);

  // the screen stays on while it shows
  let lock = null;
  const keepAwake = () => {
    if (document.visibilityState === 'visible') {
      navigator.wakeLock?.request('screen').then((l) => { lock = l; }).catch(() => {});
    }
  };
  keepAwake();
  document.addEventListener('visibilitychange', keepAwake);

  // Back closes it (on phones especially)
  history.pushState({ visualiser: true }, '');
  const onPop = () => close(true);
  window.addEventListener('popstate', onPop);

  function close(fromHistory = false) {
    if (open?.layer !== layer) return;
    open = null;
    viz.stop();
    overlay.destroy();
    offs.forEach((f) => f());
    clearInterval(ticker);
    clearTimeout(idle);
    window.removeEventListener('keydown', keys);
    window.removeEventListener('popstate', onPop);
    document.removeEventListener('visibilitychange', keepAwake);
    lock?.release().catch(() => {});
    if (document.fullscreenElement) document.exitFullscreen?.().catch(() => {});
    layer.remove();
    document.body.classList.remove('viz-open');
    if (!fromHistory && history.state?.visualiser) history.back();
  }
  return { layer, close };
}
