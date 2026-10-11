// RMS web UI – application shell: polling, routing, header, theme, keyboard.

import { rpc, playlistControl } from './api.js';
import { store, on, emit, currentPlayer, elapsed, patchStatus } from './store.js';
import { $, $$, h, toast, storage, debounce, hydrateIcons, setIcon, isTyping } from './util.js';
import * as NowPlaying from './views/nowplaying.js';
import * as Queue from './views/queue.js';
import * as Library from './views/library.js';
import * as Search from './views/search.js';
import * as Settings from './views/settings.js';

const STATUS_TAGS = 'tags:aAlcdegKjJNostuy';
const QUEUE_TAGS = 'tags:aAlcdeKostu';
const STATUS_INTERVAL = 1000;
const SERVER_INTERVAL = 5000;
const SERVER_FAST_INTERVAL = 1000;
const mobile = matchMedia('(max-width: 899px)');

// =============================================================== commands

/** Send a command to the selected player; toasts on failure, resolves to result or null. */
async function cmd(...args) {
  const pid = store.playerId;
  if (!pid) { toast('No player selected', 'error'); return null; }
  try {
    const r = await rpc(args, pid);
    refreshSoon();
    return r;
  } catch (e) {
    toast(`Command “${args.slice(0, 2).join(' ')}” failed: ${e.message}`, 'error');
    return null;
  }
}

/** Send a server-level command (no player). */
async function serverCmd(...args) {
  try { return await rpc(args, ''); } catch (e) {
    toast(`Command “${args.slice(0, 2).join(' ')}” failed: ${e.message}`, 'error');
    return null;
  }
}

const ACTION_VERB = { load: 'Playing', add: 'Added to queue', insert: 'Playing next' };

/** playlistcontrol wrapper used by library views. action = load|add|insert */
async function playItem(action, key, value, label = '') {
  const pid = store.playerId;
  if (!pid) { toast('Select a player first', 'error'); return false; }
  try {
    await playlistControl(pid, action, key, value);
    if (action === 'load' && store.status && store.status.power === 0) await rpc(['power', 1], pid).catch(() => {});
    toast(label ? `${ACTION_VERB[action]}: ${label}` : ACTION_VERB[action]);
    queueDirty = true;
    refreshSoon();
    return true;
  } catch (e) {
    toast(`Could not ${action === 'load' ? 'play' : 'add'} ${label || 'item'}: ${e.message}`, 'error');
    return false;
  }
}

function togglePlay() {
  const st = store.status;
  if (!st) return;
  if (st.mode === 'play') { patchStatus({ mode: 'pause', time: elapsed() }); cmd('pause', '1'); }
  else if (st.mode === 'pause') { patchStatus({ mode: 'play', time: elapsed() }); cmd('pause', '0'); }
  else cmd('play');
}

// =============================================================== polling

let statusTimer = 0, statusBusy = false, statusAgain = false;
let serverTimer = 0, serverBusy = false, fastServerUntil = 0;
let queueKey = null, queueFetchedAt = 0, queueDirty = true;
let lastError = '';

const visible = () => document.visibilityState === 'visible';

function setOnline(ok, err) {
  if (ok === store.online) return;
  store.online = ok;
  $('#offline-banner').hidden = ok;
  if (!ok) {
    lastError = err?.message || 'unknown error';
    toast(`Lost connection to server: ${lastError}`, 'error');
  } else {
    toast('Reconnected to server');
  }
  emit('online', ok);
}

async function pollStatus() {
  clearTimeout(statusTimer);
  if (statusBusy) { statusAgain = true; return; }
  const pid = store.playerId;
  if (!pid) return;
  statusBusy = true;
  try {
    const st = await rpc(['status', '-', 1, STATUS_TAGS], pid);
    if (pid !== store.playerId) return;
    store.status = st;
    store.statusAt = performance.now();
    setOnline(true);
    const key = `${st.playlist_timestamp ?? ''}|${st.playlist_tracks ?? 0}`;
    const stale = st.playlist_timestamp == null && Date.now() - queueFetchedAt > 5000;
    if (queueDirty || key !== queueKey || stale) {
      queueKey = key;
      await refreshQueue();
    }
    emit('status', st);
  } catch (e) {
    if (e.network) setOnline(false, e);
    else if (e.message !== lastError) { lastError = e.message; console.warn('status failed', e); }
  } finally {
    statusBusy = false;
    if (statusAgain) { statusAgain = false; statusTimer = setTimeout(pollStatus, 0); }
    else if (visible()) statusTimer = setTimeout(pollStatus, STATUS_INTERVAL);
  }
}

async function refreshQueue() {
  const pid = store.playerId;
  if (!pid) return;
  queueDirty = false;
  try {
    const r = await rpc(['status', 0, 999, QUEUE_TAGS], pid);
    if (pid !== store.playerId) return;
    store.queue = r.playlist_loop || [];
    queueFetchedAt = Date.now();
    emit('queue', store.queue);
  } catch (e) {
    queueDirty = true;
    if (!e.network) toast(`Could not load queue: ${e.message}`, 'error');
  }
}

let soonTimer = 0;
/** Re-poll status shortly after a command so the UI catches up quickly. */
function refreshSoon(ms = 200, { queue = false } = {}) {
  if (queue) queueDirty = true;
  clearTimeout(soonTimer);
  soonTimer = setTimeout(pollStatus, ms);
}

async function pollServer() {
  clearTimeout(serverTimer);
  if (serverBusy) return;
  serverBusy = true;
  try {
    const r = await rpc(['serverstatus', 0, 100]);
    setOnline(true);
    store.server = r;
    const players = r.players_loop || [];
    const sig = (list) => list.map((p) => [p.playerid, p.name, p.connected, p.power, p.model].join('|')).join(';');
    const changed = sig(players) !== sig(store.players);
    store.players = players;
    if (changed) { emit('players', players); ensurePlayer(); }
    emit('server', r);
  } catch (e) {
    if (e.network) setOnline(false, e);
  } finally {
    serverBusy = false;
    if (visible()) {
      const fast = Date.now() < fastServerUntil || +store.server?.rescan > 0;
      serverTimer = setTimeout(pollServer, fast ? SERVER_FAST_INTERVAL : SERVER_INTERVAL);
    }
  }
}

/** Poll serverstatus every second for a while (e.g. after starting a rescan). */
function fastServerPoll(ms = 15000) {
  fastServerUntil = Date.now() + ms;
  pollServer();
}

document.addEventListener('visibilitychange', () => {
  if (visible()) { pollServer(); pollStatus(); }
  else { clearTimeout(statusTimer); clearTimeout(serverTimer); }
});

// =============================================================== players

let autoPicked = false;

function ensurePlayer() {
  const ids = store.players.map((p) => p.playerid);
  const saved = storage.get('rms.player');
  if (store.playerId && ids.includes(store.playerId) && !(autoPicked && saved && ids.includes(saved) && saved !== store.playerId)) return;
  let pick = ids.includes(saved) ? saved : null;
  autoPicked = !pick;
  if (!pick) {
    const p = store.players.find((x) => +x.isplaying) || store.players.find((x) => +x.connected) || store.players[0];
    pick = p?.playerid || null;
  }
  selectPlayer(pick);
}

function selectPlayer(id, byUser = false) {
  if (byUser) { storage.set('rms.player', id); autoPicked = false; }
  if (id === store.playerId) return;
  store.playerId = id;
  store.status = null;
  store.queue = [];
  queueKey = null;
  queueDirty = true;
  emit('player', id);
  emit('queue', store.queue);
  emit('status', null);
  renderPlayerSelect();
  if (id) pollStatus();
}

function renderPlayerSelect() {
  const sel = $('#player-select');
  const players = store.players;
  if (!players.length) {
    sel.replaceChildren(h('option', { value: '' }, store.server ? 'No players' : 'Loading players…'));
    sel.disabled = true;
    return;
  }
  sel.disabled = false;
  sel.replaceChildren(...players.map((p) => h('option', { value: p.playerid, selected: p.playerid === store.playerId },
    `${p.name || p.playerid}${+p.connected === 0 ? ' (disconnected)' : ''}`)));
  sel.value = store.playerId || '';
  const cur = currentPlayer();
  sel.title = cur ? `${cur.modelname || cur.model || 'Player'} · ${cur.ip || ''} · ${cur.playerid}` : '';
}

function renderPower() {
  const btn = $('#power-btn');
  const st = store.status;
  const power = st ? +st.power === 1 : +currentPlayer()?.power === 1;
  btn.disabled = !store.playerId;
  btn.setAttribute('aria-pressed', String(power));
  btn.setAttribute('aria-label', power ? 'Turn player off' : 'Turn player on');
  btn.title = power ? 'Player is on – click to turn off' : 'Player is off – click to turn on';
  btn.classList.toggle('on', power);
}

// =============================================================== theme

const darkMQ = matchMedia('(prefers-color-scheme: dark)');

function getTheme() { return storage.get('rms.theme', 'auto'); }
function effectiveTheme() {
  const t = getTheme();
  return t === 'dark' || (t !== 'light' && darkMQ.matches) ? 'dark' : 'light';
}
function setTheme(t) {
  if (t !== 'light' && t !== 'dark') t = 'auto';
  storage.set('rms.theme', t === 'auto' ? null : t);
  applyTheme();
}
function applyTheme() {
  const t = getTheme();
  if (t === 'light' || t === 'dark') document.documentElement.dataset.theme = t;
  else delete document.documentElement.dataset.theme;
  const eff = effectiveTheme();
  const btn = $('#theme-btn');
  setIcon(btn, eff === 'dark' ? 'sun' : 'moon');
  btn.setAttribute('aria-label', eff === 'dark' ? 'Switch to light theme' : 'Switch to dark theme');
  btn.title = btn.getAttribute('aria-label');
  emit('theme', t);
}
function toggleTheme() {
  const next = effectiveTheme() === 'dark' ? 'light' : 'dark';
  setTheme(next === (darkMQ.matches ? 'dark' : 'light') ? 'auto' : next);
}
darkMQ.addEventListener('change', applyTheme);

// =============================================================== routing

const viewCache = new Map(); // hash -> {key, node, scroll, cleanup}
const VIEW_CACHE_SIZE = 6;
let current = null;

function parseHash() {
  const raw = location.hash.replace(/^#\/?/, '');
  const [path, qs = ''] = raw.split('?');
  const parts = path.split('/').filter(Boolean).map((s) => { try { return decodeURIComponent(s); } catch { return s; } });
  return { parts, query: new URLSearchParams(qs), key: raw };
}

function route() {
  const { parts, query, key } = parseHash();
  if (!parts.length) {
    location.replace(mobile.matches ? '#/now' : '#/library/albums');
    return;
  }
  const top = parts[0];
  const section = top === 'now' || top === 'queue' ? top : 'main';
  document.body.dataset.section = section;
  for (const a of $$('.bottom-nav a')) {
    const nav = a.dataset.nav;
    const active = nav === top || (nav === 'library' && (top === 'library' || top === 'search'));
    if (active) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current');
  }
  $('#settings-link').classList.toggle('active', top === 'settings');
  if (section !== 'main') {
    if (!current) renderMain('library/albums', ['library', 'albums'], new URLSearchParams());
    if (section === 'queue') Queue.revealCurrent();
    return;
  }
  renderMain(key, parts, query);
}

function renderMain(key, parts, query) {
  const main = $('#main');
  if (current?.key === key) return;
  if (current) {
    current.scroll = main.scrollTop;
    if (current.cacheable) {
      viewCache.set(current.key, current);
      while (viewCache.size > VIEW_CACHE_SIZE) {
        const [k, old] = viewCache.entries().next().value;
        viewCache.delete(k);
        old.cleanup?.();
      }
    } else current.cleanup?.();
  }
  main.replaceChildren();
  const cached = viewCache.get(key);
  if (cached) {
    viewCache.delete(key);
    main.append(cached.node);
    main.scrollTop = cached.scroll;
    current = cached;
    cached.onShow?.();
    return;
  }
  const view = parts[0] === 'settings' ? Settings : parts[0] === 'search' ? Search : Library;
  const node = h('div', { class: 'view' });
  main.append(node);
  main.scrollTop = 0;
  const entry = { key, node, scroll: 0, cacheable: view.cacheable !== false, cleanup: null };
  current = entry;
  try {
    const r = view.render(node, { parts, query, ctx, scrollRoot: main });
    entry.cleanup = typeof r === 'function' ? r : r?.cleanup;
    entry.onShow = r?.onShow;
  } catch (e) {
    console.error(e);
    node.append(h('p', { class: 'empty error' }, `Something went wrong: ${e.message}`));
  }
}

/** Drop the cached copy of the current view and render it again. */
function reload() {
  if (current) { current.cleanup?.(); current = null; }
  viewCache.clear();
  route();
}

/** Forget cached library views (e.g. after a rescan). */
function invalidateViews() {
  for (const v of viewCache.values()) v.cleanup?.();
  viewCache.clear();
}

function navigate(hash, { replace = false } = {}) {
  if (replace) { history.replaceState(null, '', hash); route(); }
  else location.hash = hash;
}

window.addEventListener('hashchange', route);
mobile.addEventListener('change', () => { if (document.body.dataset.section !== 'main' && !mobile.matches) route(); });

// =============================================================== search box

function setupSearch() {
  const input = $('#search-input');
  const go = () => {
    const q = input.value.trim();
    const onSearch = location.hash.startsWith('#/search');
    if (!q) { if (onSearch) history.back(); return; }
    if (q.length < 2) return;
    const target = `#/search?q=${encodeURIComponent(q)}`;
    if (location.hash === target) return;
    navigate(target, { replace: onSearch });
  };
  const debounced = debounce(go, 300);
  input.addEventListener('input', debounced);
  $('#search-form').addEventListener('submit', (e) => { e.preventDefault(); debounced.cancel(); go(); input.blur(); });
  input.addEventListener('keydown', (e) => { if (e.key === 'Escape') { input.value = ''; debounced.cancel(); go(); input.blur(); } });
}

// =============================================================== keyboard

document.addEventListener('keydown', (e) => {
  if (e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey) return;
  const t = e.target;
  if (isTyping(t)) return;
  if (e.key === ' ' || e.code === 'Space') {
    // Let focused buttons/links handle space themselves.
    if (t.closest?.('button, a[href], [role=button], summary, input')) return;
    e.preventDefault();
    togglePlay();
  } else if (e.key === '/') {
    e.preventDefault();
    $('#search-input').focus();
  }
});

/** Play a mix of songs similar to a track / album / artist / genre / year (`createmix`). */
async function createMix(key, value, label = '') {
  const pid = store.playerId;
  if (!pid) { toast('Select a player first', 'error'); return false; }
  try {
    const r = await rpc(['createmix', 'play', `${key}:${value}`], pid);
    if (!+r.count) { toast(`No songs to mix with ${label || 'this'}`, 'error'); return false; }
    if (store.status && store.status.power === 0) await rpc(['power', 1], pid).catch(() => {});
    toast(label ? `Playing a mix: ${label}` : 'Playing a mix');
    queueDirty = true;
    refreshSoon();
    return true;
  } catch (e) {
    toast(`Could not create a mix: ${e.message}`, 'error');
    return false;
  }
}

// =============================================================== context passed to views

export const ctx = {
  cmd, serverCmd, playItem, createMix, togglePlay,
  refreshSoon, refreshQueue: () => { queueDirty = true; return pollStatus(); },
  refreshServer: pollServer, fastServerPoll,
  selectPlayer, navigate, reload, invalidateViews,
  getTheme, setTheme,
};

// =============================================================== boot

function boot() {
  hydrateIcons();
  applyTheme();
  setupSearch();

  $('#player-select').addEventListener('change', (e) => selectPlayer(e.target.value, true));
  $('#power-btn').addEventListener('click', () => {
    const on = +store.status?.power === 1;
    patchStatus({ power: on ? 0 : 1 });
    renderPower();
    cmd('power', on ? 0 : 1);
  });
  $('#theme-btn').addEventListener('click', toggleTheme);

  NowPlaying.init($('#nowplaying'), $('#mini'), ctx);
  Queue.init($('#queue'), ctx);

  on('players', () => { renderPlayerSelect(); renderPower(); });
  on('status', renderPower);
  on('server', (r) => {
    // A finished rescan invalidates cached library pages.
    const scanning = +r.rescan > 0;
    if (wasScanning && !scanning) invalidateViews();
    wasScanning = scanning;
    if (!store.players.length) renderPlayerSelect();
  });

  route();
  pollServer();
}
let wasScanning = false;

if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', boot);
else boot();
