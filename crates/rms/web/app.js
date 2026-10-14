// RMS web UI – application shell: live state, routing, header, theme, keyboard.

import { command, getPlayer, getQueue, connectEvents, itemFor } from './api.js';
import { store, on, emit, currentPlayer, elapsed, patchPlayer } from './store.js';
import { $, $$, h, toast, storage, debounce, hydrateIcons, setIcon, isTyping } from './util.js';
import * as NowPlaying from './views/nowplaying.js';
import * as Queue from './views/queue.js';
import * as Library from './views/library.js';
import * as Search from './views/search.js';
import * as Settings from './views/settings.js';
import { initBrowserPlayer } from './browser-player.js';
import { openMenu } from './views/menu.js';

const mobile = matchMedia('(max-width: 899px)');

// =============================================================== commands

/**
 * Send a native command to the selected player (`{command: 'pause'}`); toasts on failure,
 * resolves to the result or null.  State changes arrive through the event stream.
 */
async function send(body, { quiet = false } = {}) {
  const pid = store.playerId;
  if (!pid) { if (!quiet) toast('No player selected', 'error'); return null; }
  try {
    return await command(pid, body);
  } catch (e) {
    if (!quiet) toast(`${body.command[0].toUpperCase()}${body.command.slice(1).replace(/_/g, ' ')} failed: ${e.message}`, 'error');
    resync(pid);
    return null;
  }
}

/** Fetch a player's state again (e.g. after an optimistic change the server refused). */
async function resync(pid) {
  try { applyPlayer(await getPlayer(pid)); } catch { /* the event stream will catch up */ }
}

const ACTION_VERB = { load: 'Playing', add: 'Added to queue', insert: 'Playing next' };
const ENQUEUE_MODE = { load: 'play', add: 'add', insert: 'next' };

/** Queue a library item.  action = load|add|insert; `start` = queue entry to start at. */
async function playItem(action, key, value, label = '', { start } = {}) {
  const pid = store.playerId;
  if (!pid) { toast('Select a player first', 'error'); return false; }
  const item = itemFor(key, value);
  if (!item) { toast(`Cannot play ${label || 'this'}`, 'error'); return false; }
  try {
    await command(pid, { command: 'enqueue', item, mode: ENQUEUE_MODE[action], start });
    toast(label ? `${ACTION_VERB[action]}: ${label}` : ACTION_VERB[action]);
    return true;
  } catch (e) {
    toast(`Could not ${action === 'load' ? 'play' : 'add'} ${label || 'item'}: ${e.message}`, 'error');
    return false;
  }
}

function togglePlay() {
  const p = currentPlayer();
  if (!p) return;
  if (p.mode === 'play') { patchPlayer({ elapsed: elapsed(), mode: 'pause' }); send({ command: 'pause' }); }
  else { patchPlayer({ elapsed: elapsed(), mode: 'play' }); send({ command: 'play' }); }
}

// =============================================================== live state

let lastError = '';

function setOnline(ok, err) {
  if (ok === store.online) return;
  store.online = ok;
  $('#offline-banner').hidden = ok;
  if (!ok) {
    lastError = err || 'connection lost';
    toast(`Lost connection to server: ${lastError}`, 'error');
  } else {
    toast('Reconnected to server');
  }
  emit('online', ok);
}

/** What the players list shows; the list re-renders only when this changes. */
const summary = (list) => list.map((p) => [p.id, p.name, p.connected, p.power, p.model].join('|')).join(';');

/** Take in a player's new state (from an event or a fetch). */
function applyPlayer(p) {
  const before = summary(store.players);
  const i = store.players.findIndex((x) => x.id === p.id);
  if (i >= 0) store.players[i] = p; else store.players.push(p);
  store.receivedAt.set(p.id, performance.now());
  emit('player-state', p);
  if (summary(store.players) !== before) { emit('players', store.players); ensurePlayer(); }
  if (p.id === store.playerId) {
    emit('status', p);
    checkQueue();
  }
}

/** Fetch the queue when its version or length differs from what we have. */
function checkQueue() {
  const p = currentPlayer();
  if (!p) return;
  const key = `${p.queue.version}|${p.queue.length}`;
  if (key !== store.queueKey) refreshQueue(key);
}

let queueSeq = 0;
async function refreshQueue(key = null) {
  const pid = store.playerId;
  if (!pid) return;
  const seq = ++queueSeq;
  store.queueKey = key;
  try {
    const q = await getQueue(pid);
    if (pid !== store.playerId || seq !== queueSeq) return;
    store.queue = q.items;
    store.queueKey = `${q.version}|${q.length}`;
    emit('queue', store.queue);
  } catch (e) {
    if (seq === queueSeq) store.queueKey = null;
    if (!e.network) toast(`Could not load queue: ${e.message}`, 'error');
  }
}

function startEvents() {
  connectEvents({
    open: () => setOnline(true),
    error: () => setOnline(false, 'reconnecting…'),
    snapshot: ({ server, players }) => {
      setOnline(true);
      store.server = server;
      const now = performance.now();
      store.players = players;
      for (const p of players) store.receivedAt.set(p.id, now);
      emit('server', server);
      emit('players', players);
      ensurePlayer();
      if (currentPlayer()) { emit('status', currentPlayer()); store.queueKey = null; checkQueue(); }
    },
    server: ({ server }) => { store.server = server; emit('server', server); },
    player: ({ player }) => applyPlayer(player),
    player_removed: ({ id }) => {
      store.players = store.players.filter((p) => p.id !== id);
      store.receivedAt.delete(id);
      emit('players', store.players);
      ensurePlayer();
    },
  });
}

// =============================================================== players

let autoPicked = false;

function ensurePlayer() {
  const ids = store.players.map((p) => p.id);
  const saved = storage.get('rms.player');
  if (store.playerId && ids.includes(store.playerId) && !(autoPicked && saved && ids.includes(saved) && saved !== store.playerId)) return;
  let pick = ids.includes(saved) ? saved : null;
  autoPicked = !pick;
  if (!pick) {
    const p = store.players.find((x) => x.mode === 'play') || store.players.find((x) => x.connected) || store.players[0];
    pick = p?.id || null;
  }
  selectPlayer(pick);
}

function selectPlayer(id, byUser = false) {
  if (byUser) { storage.set('rms.player', id); autoPicked = false; }
  if (id === store.playerId) return;
  store.playerId = id;
  store.queue = [];
  store.queueKey = null;
  emit('player', id);
  emit('queue', store.queue);
  emit('status', currentPlayer());
  renderPlayerSelect();
  checkQueue();
}

/** Kinds of players, in the order the picker lists them: [kind, heading, icon]. */
const PLAYER_KINDS = [
  ['squeezebox', 'Squeezebox', 'speaker'],
  ['chromecast', 'Chromecast', 'cast'],
  ['airplay', 'AirPlay', 'airplay'],
  ['upnp', 'UPnP / DLNA', 'router'],
  ['browser', 'Web', 'laptop'],
];

/** Squeezeboxes and everything speaking slimproto (squeezelite, WiiM, …) are `squeezebox`. */
function playerKind(p) {
  return ['chromecast', 'airplay', 'upnp', 'browser'].includes(p.model) ? p.model : 'squeezebox';
}

const kindIcon = (p) => PLAYER_KINDS.find(([k]) => k === playerKind(p))[2];

function renderPlayerSelect() {
  const btn = $('#player-select');
  const players = store.players;
  const name = btn.querySelector('.player-button-name');
  if (!players.length) {
    name.textContent = store.server ? 'No players' : 'Loading players…';
    btn.disabled = true;
    btn.title = '';
    return;
  }
  btn.disabled = false;
  const cur = currentPlayer();
  name.textContent = cur ? cur.name || cur.id : 'Choose a player';
  setIcon(btn, cur ? kindIcon(cur) : 'speaker');
  btn.setAttribute('aria-label', `Player: ${cur ? cur.name || cur.id : 'none'}. Choose a player`);
  btn.title = cur ? [cur.model_name || cur.model || 'Player', cur.ip, cur.id].filter(Boolean).join(' · ') : '';
}

/** The player picker: players grouped by kind, each kind under its heading. */
function openPlayerMenu() {
  const btn = $('#player-select');
  const byName = (a, b) => (a.name || a.id).localeCompare(b.name || b.id, undefined, { numeric: true });
  const groups = PLAYER_KINDS
    .map(([kind, heading, ic]) => [heading, ic, store.players.filter((p) => playerKind(p) === kind).sort(byName)])
    .filter(([, , ps]) => ps.length);
  const entries = groups.flatMap(([heading, ic, ps]) => [
    groups.length > 1 ? { heading, icon: ic } : null,
    ...ps.map((p) => ({
      label: p.name || p.id,
      icon: groups.length > 1 ? null : ic,
      detail: !p.connected ? 'disconnected' : !p.power ? 'off' : p.mode === 'play' ? 'playing' : null,
      checked: p.id === store.playerId,
      run: () => selectPlayer(p.id, true),
    })),
  ]);
  btn.setAttribute('aria-expanded', 'true');
  openMenu(btn, 'Players', entries, { align: 'start', onClose: () => btn.setAttribute('aria-expanded', 'false') });
}

function renderPower() {
  const btn = $('#power-btn');
  const power = !!currentPlayer()?.power;
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

/** Play a mix of songs similar to a track / album / artist / genre / year. */
async function createMix(key, value, label = '') {
  const pid = store.playerId;
  if (!pid) { toast('Select a player first', 'error'); return false; }
  try {
    const r = await command(pid, { command: 'mix', seed: itemFor(key, value) });
    if (!r?.count) { toast(`No songs to mix with ${label || 'this'}`, 'error'); return false; }
    toast(label ? `Playing a mix: ${label}` : 'Playing a mix');
    return true;
  } catch (e) {
    toast(`Could not create a mix: ${e.message}`, 'error');
    return false;
  }
}

// =============================================================== context passed to views

export const ctx = {
  send, playItem, createMix, togglePlay,
  refreshQueue: () => refreshQueue(),
  selectPlayer, navigate, reload, invalidateViews,
  getTheme, setTheme,
};

// =============================================================== boot

function boot() {
  hydrateIcons();
  applyTheme();
  setupSearch();

  $('#player-select').addEventListener('click', openPlayerMenu);
  $('#power-btn').addEventListener('click', () => {
    const on = !!currentPlayer()?.power;
    patchPlayer({ power: !on });
    renderPower();
    send({ command: 'power', on: !on });
  });
  $('#theme-btn').addEventListener('click', toggleTheme);

  NowPlaying.init($('#nowplaying'), $('#mini'), ctx);
  Queue.init($('#queue'), ctx);

  on('players', () => { renderPlayerSelect(); renderPower(); });
  on('status', renderPower);
  on('server', (r) => {
    // A finished rescan invalidates cached library pages.
    const scanning = !!r.scan;
    if (wasScanning && !scanning) invalidateViews();
    wasScanning = scanning;
    if (!store.players.length) renderPlayerSelect();
  });

  // this browser as a player: select it when it was just turned on
  on('browser-player', (b) => {
    if (b.playerId && b.selectWhenConnected) {
      b.selectWhenConnected = false;
      const pick = () => {
        if (!store.players.some((p) => p.id === b.playerId)) return false;
        selectPlayer(b.playerId, true);
        return true;
      };
      if (!pick()) { const off = on('players', () => { if (pick()) off(); }); }
    }
  });
  initBrowserPlayer();

  route();
  startEvents();
}
let wasScanning = false;

// installable as an app, and opens without the network (sw.js)
if ('serviceWorker' in navigator && window.isSecureContext) {
  navigator.serviceWorker.register('./sw.js', { scope: './' }).catch((e) => console.warn('service worker:', e));
}

if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', boot);
else boot();
