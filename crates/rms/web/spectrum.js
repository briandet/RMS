// Spectrum bars: four bars (bass, low mids, high mids, treble) moving with what a player
// plays.  The server works the levels out ahead of time (a library track's whole curve) or as
// a stream plays (radio, Spotify); here they are played back from the player's position, so
// no audio reaches the browser.  See `GET /api/v1/players/{id}/spectrum` in docs/api.md.
//
// Where there is no curve (yet), and with reduced motion, the bars bounce by themselves.

import { api } from './api.js';
import { store, playerElapsed } from './store.js';
import { h } from './util.js';

/** Bars fall back this much of their height a second (they rise at once). */
const FALL = 2;
/** A live stream's readings are fetched this often (ms). */
const LIVE_POLL = 1000;
/** Without a curve, ask again after this long (ms): an unanalysed track gets one. */
const RETRY = 15000;
/** Lowest bar, as a share of the height. */
const FLOOR = 0.15;

const reduced = matchMedia('(prefers-reduced-motion: reduce)');

/** Player id → { what, kind, fps, start, levels, at, busy }. */
const curves = new Map();
/** Bars on the page: { el, bars, player(), shown }. */
const mounted = new Set();
/** A tick is scheduled. */
let scheduled = false;
let last = 0;

/**
 * Bars for a player (its id, or a function giving the id, for a view that follows the
 * selected player).  They animate while on the page.
 */
export function spectrumBars(player, cls = '') {
  const el = h('span', { class: `eq-bars ${cls}`.trim(), role: 'img', 'aria-label': 'playing', title: 'Playing' },
    h('i'), h('i'), h('i'), h('i'));
  mounted.add({ el, bars: [...el.children], player: typeof player === 'function' ? player : () => player, shown: [0, 0, 0, 0] });
  schedule(true);
  return el;
}

/** What a player plays, for telling when its curve no longer fits. */
function playing(p) {
  const t = p.now_playing;
  return t ? `${t.id ?? t.url}|${p.queue?.index ?? ''}` : null;
}

function isPlaying(p) {
  return !!p && p.connected && p.power && p.mode === 'play';
}

function unpack(b64) {
  const bin = atob(b64);
  const levels = new Uint8Array(bin.length * 2);
  for (let i = 0; i < bin.length; i++) {
    const c = bin.charCodeAt(i);
    levels[2 * i] = c >> 4;
    levels[2 * i + 1] = c & 15;
  }
  return levels;
}

async function fetchCurve(p, what, now) {
  const old = curves.get(p.id);
  if (old?.busy) return;
  const c = old?.what === what ? old : { what, kind: 'none' };
  c.busy = true;
  curves.set(p.id, c);
  try {
    const from = Math.max(0, playerElapsed(p));
    const s = await api(`players/${encodeURIComponent(p.id)}/spectrum?from=${from.toFixed(2)}`);
    Object.assign(c, { kind: s.kind, fps: s.fps, start: s.start, levels: unpack(s.curve) });
  } catch {
    c.kind = 'none';
  } finally {
    c.busy = false;
    c.at = now;
  }
}

/** Fetch the curves the bars on the page need. */
function refresh(now) {
  const seen = new Set();
  for (const m of mounted) {
    const p = store.players.find((x) => x.id === m.player());
    if (!isPlaying(p) || seen.has(p.id)) continue;
    seen.add(p.id);
    const what = playing(p);
    const c = curves.get(p.id);
    if (!c || c.what !== what) fetchCurve(p, what, now);
    else if (c.busy) continue;
    else if (c.kind === 'live' && now - c.at > LIVE_POLL) fetchCurve(p, what, now);
    else if (c.kind === 'none' && now - c.at > RETRY) fetchCurve(p, what, now);
  }
}

/** Level 0–1 of band `b` at `t` seconds into the track, between readings. */
function level(c, t, b) {
  const n = c.levels.length / 4;
  const f = (t - c.start) * c.fps;
  if (!(f >= 0 && f < n)) return 0;
  const i = Math.floor(f);
  const j = Math.min(i + 1, n - 1);
  const k = f - i;
  return (c.levels[i * 4 + b] * (1 - k) + c.levels[j * 4 + b] * k) / 15;
}

/** Every frame while bars move; twice a second while none does (to notice play). */
function schedule(moving) {
  if (scheduled) return;
  scheduled = true;
  const run = () => requestAnimationFrame(tick);
  if (moving) run();
  else setTimeout(run, 500);
}

function tick(now) {
  scheduled = false;
  for (const m of mounted) if (!m.el.isConnected && m.el.dataset.seen) mounted.delete(m);
  if (!mounted.size) return;
  const dt = last ? Math.min((now - last) / 1000, 0.25) : 0;
  last = now;
  refresh(now);
  let moving = false;
  for (const m of mounted) {
    if (!m.el.isConnected) continue;
    m.el.dataset.seen = '1';
    const p = store.players.find((x) => x.id === m.player());
    const on = isPlaying(p);
    const c = on ? curves.get(p.id) : null;
    const live = !!c && c.what === playing(p) && c.kind !== 'none' && !!c.levels && !reduced.matches;
    // no curve: the bars bounce by themselves (CSS), while playing
    m.el.classList.toggle('deco', on && !live);
    m.el.classList.toggle('driven', live || !on);
    // gone once the player stops and the bars have fallen
    m.el.hidden = !on && m.shown.every((v) => v === 0);
    if (!live && on) continue;
    if (live) moving = true;
    const t = on ? playerElapsed(p) : 0;
    for (let b = 0; b < 4; b++) {
      const target = live ? level(c, t, b) : 0;
      m.shown[b] = Math.max(target, m.shown[b] - FALL * dt);
      if (m.shown[b] > 0) moving = true;
      m.bars[b].style.transform = `scaleY(${(FLOOR + (1 - FLOOR) * m.shown[b]).toFixed(3)})`;
    }
  }
  if (!moving) last = 0;
  schedule(moving);
}
