// Now Playing panel (side column on desktop, own tab on mobile) and the mobile mini player.

import { store, on, elapsed, currentTrack, currentPlayer, patchStatus } from '../store.js';
import { h, icon, setIcon, fmtTime, throttle, toast } from '../util.js';
import { resolveArtwork, currentArtUrl } from '../api.js';
import { connectHelp, enc } from './common.js';

const SHUFFLE = ['off', 'by song', 'by album'];
const REPEAT = ['off', 'one track', 'whole queue'];
const REPEAT_NEXT = [2, 0, 1]; // off -> all -> one -> off

let ctx;
const el = {};
const mini = {};
let seeking = false;
let volumeHoldUntil = 0;

export function init(root, miniRoot, _ctx) {
  ctx = _ctx;
  buildPanel(root);
  buildMini(miniRoot);
  on('status', render);
  on('players', render);
  on('player', render);
  on('server', () => { if (!store.players.length) render(); });
  render();
  setInterval(tick, 250);
}

// ---------------------------------------------------------------- build

function buildPanel(root) {
  const btn = (name, label, onclick, cls = '') =>
    h('button', { class: `icon-btn ${cls}`.trim(), type: 'button', 'aria-label': label, title: label, onclick }, icon(name));

  el.empty = h('div', { class: 'np-empty', hidden: true });
  el.notice = h('div', { class: 'np-notice', hidden: true });

  el.img = h('img', { alt: '', decoding: 'async' });
  el.img.addEventListener('load', () => el.art.classList.add('loaded'));
  el.img.addEventListener('error', () => el.art.classList.remove('loaded'));
  el.art = h('div', { class: 'art np-art' }, icon('album', 'icon art-fallback'), el.img);

  el.title = h('h2', { class: 'np-title' });
  el.artist = h('div', { class: 'np-artist' });
  el.album = h('div', { class: 'np-album' });
  el.fav = btn('heart', 'Add to favorites', addFavorite, 'np-fav');

  el.elapsed = h('span', { class: 'time' }, '0:00');
  el.duration = h('span', { class: 'time' }, '0:00');
  el.seek = h('input', { type: 'range', class: 'range seek', min: 0, max: 0, step: 1, value: 0, 'aria-label': 'Seek' });
  el.seek.addEventListener('input', () => {
    seeking = true;
    el.elapsed.textContent = fmtTime(el.seek.value);
    fill(el.seek);
  });
  el.seek.addEventListener('change', () => {
    const t = +el.seek.value;
    seeking = false;
    patchStatus({ time: t });
    ctx.cmd('time', t);
  });

  el.shuffle = btn('shuffle', 'Shuffle', cycleShuffle, 'toggle');
  el.prev = btn('prev', 'Previous track', () => ctx.cmd('playlist', 'index', '-1'));
  el.play = btn('play', 'Play', () => ctx.togglePlay(), 'play-btn');
  el.next = btn('next', 'Next track', () => ctx.cmd('playlist', 'index', '+1'));
  el.repeat = btn('repeat', 'Repeat', cycleRepeat, 'toggle');
  el.repeatBadge = h('span', { class: 'badge', hidden: true }, '1');
  el.repeat.append(el.repeatBadge);

  el.mute = btn('volume', 'Mute', toggleMute);
  el.volume = h('input', { type: 'range', class: 'range volume', min: 0, max: 100, step: 1, value: 0, 'aria-label': 'Volume' });
  el.volValue = h('span', { class: 'vol-value', 'aria-hidden': 'true' }, '0');
  const sendVolume = throttle((v) => ctx.cmd('mixer', 'volume', v), 150);
  el.volume.addEventListener('input', () => {
    volumeHoldUntil = Date.now() + 2000;
    const v = +el.volume.value;
    el.volValue.textContent = v;
    fill(el.volume);
    if (store.status) store.status['mixer volume'] = v;
    sendVolume(v);
  });
  el.volume.addEventListener('change', () => { volumeHoldUntil = Date.now() + 1500; });

  el.body = h('div', { class: 'np-body' },
    el.notice,
    el.art,
    h('div', { class: 'np-meta' },
      h('div', { class: 'np-text' }, el.title, el.artist, el.album),
      el.fav),
    h('div', { class: 'np-seek' }, el.elapsed, el.seek, el.duration),
    h('div', { class: 'np-controls' }, el.shuffle, el.prev, el.play, el.next, el.repeat),
    h('div', { class: 'np-volume' }, el.mute, el.volume, el.volValue));

  root.append(el.empty, el.body);
}

function buildMini(root) {
  mini.root = root;
  mini.progress = h('div', { class: 'mini-progress' }, h('span'));
  mini.img = h('img', { alt: '', decoding: 'async' });
  mini.img.addEventListener('error', () => mini.img.removeAttribute('src'));
  mini.title = h('span', { class: 'mini-title' });
  mini.artist = h('span', { class: 'mini-artist' });
  mini.play = h('button', { class: 'icon-btn', type: 'button', 'aria-label': 'Play', onclick: () => ctx.togglePlay() }, icon('play'));
  mini.next = h('button', { class: 'icon-btn', type: 'button', 'aria-label': 'Next track', onclick: () => ctx.cmd('playlist', 'index', '+1') }, icon('next'));
  root.append(mini.progress,
    h('a', { class: 'mini-main', href: '#/now', 'aria-label': 'Open now playing' },
      h('span', { class: 'art mini-art' }, icon('album', 'icon art-fallback'), mini.img),
      h('span', { class: 'mini-text' }, mini.title, mini.artist)),
    mini.play, mini.next);
}

// ---------------------------------------------------------------- render

/** Title / artist / album lines, handling radio streams. */
function describe(st, tr) {
  const remote = +st.remote === 1 || +tr?.remote === 1;
  const meta = st.remoteMeta || {};
  if (remote) {
    const station = tr?.remote_title || meta.remote_title || '';
    let title = meta.title || tr?.title || st.current_title || station || 'Stream';
    let artist = meta.artist || tr?.artist || '';
    // Many stations only send "Artist - Title" as the stream title.
    if (!artist && st.current_title && / - /.test(st.current_title) && title === st.current_title) {
      [artist, title] = st.current_title.split(/ - (.+)/);
    }
    let album = meta.album || tr?.album || '';
    if (station && station !== title) album = album ? `${album} · ${station}` : station;
    return { title, artist, album, remote };
  }
  return { title: tr?.title || st.current_title || '', artist: tr?.artist || '', album: tr?.album || '', remote };
}

function artUrl(st, tr) {
  const meta = st.remoteMeta || {};
  const remoteArt = meta.artwork_url || (+st.remote === 1 && tr?.artwork_url);
  if (remoteArt) return resolveArtwork(remoteArt);
  return currentArtUrl(store.playerId, `${tr?.id ?? ''}-${tr?.coverid ?? ''}-${st.current_title || tr?.title || ''}`);
}

function render() {
  const st = store.status;
  if (!store.players.length) {
    showEmpty(store.server ? connectHelp() : h('p', { class: 'muted' }, 'Connecting to server…'));
    renderMini(null);
    return;
  }
  if (!store.playerId || !st) {
    showEmpty(h('p', { class: 'muted' }, store.playerId ? 'Loading player…' : 'Select a player.'));
    renderMini(null);
    return;
  }
  el.empty.hidden = true;
  el.body.hidden = false;

  const tr = currentTrack();
  const hasTrack = !!tr || (+st.playlist_tracks || 0) > 0 || +st.remote === 1;
  const d = describe(st, tr);

  // Notices: player off / disconnected / empty queue
  const player = currentPlayer();
  let notice = null;
  if (+st.player_connected === 0 || (player && +player.connected === 0)) notice = 'Player is disconnected.';
  else if (+st.power === 0) notice = h('span', {}, 'Player is off. ', h('button', { class: 'btn btn-small', type: 'button', onclick: () => ctx.cmd('power', 1) }, 'Turn on'));
  el.notice.replaceChildren(notice || '');
  el.notice.hidden = !notice;

  if (hasTrack) {
    el.title.textContent = d.title || 'Unknown title';
    setLine(el.artist, d.artist, tr?.artist_id != null && !d.remote ? `#/library/artist/${enc(tr.artist_id)}?name=${enc(d.artist)}` : null);
    setLine(el.album, d.album, tr?.album_id != null && !d.remote ? `#/library/album/${enc(tr.album_id)}?name=${enc(d.album)}` : null);
    const src = artUrl(st, tr);
    if (el.img.dataset.src !== src) {
      el.img.dataset.src = src;
      el.art.classList.remove('loaded');
      el.img.src = src;
    }
    el.img.alt = d.album ? `Cover of ${d.album}` : '';
  } else {
    el.title.textContent = 'Nothing playing';
    setLine(el.artist, 'Pick some music from the library to get started.');
    setLine(el.album, '');
    el.img.removeAttribute('src');
    delete el.img.dataset.src;
    el.art.classList.remove('loaded');
  }
  el.art.classList.toggle('radio', d.remote);
  el.fav.hidden = !(tr?.url || d.remote);

  // Transport
  const playing = st.mode === 'play';
  setIcon(el.play, playing ? 'pause' : 'play');
  el.play.setAttribute('aria-label', playing ? 'Pause' : 'Play');
  el.play.title = playing ? 'Pause (space)' : 'Play (space)';
  for (const b of [el.prev, el.next, el.play]) b.disabled = !hasTrack;

  const shuffle = +st['playlist shuffle'] || 0;
  el.shuffle.setAttribute('aria-pressed', String(shuffle > 0));
  el.shuffle.setAttribute('aria-label', `Shuffle: ${SHUFFLE[shuffle]}`);
  el.shuffle.title = `Shuffle: ${SHUFFLE[shuffle]}`;
  el.shuffle.dataset.mode = shuffle;

  const repeat = +st['playlist repeat'] || 0;
  el.repeat.setAttribute('aria-pressed', String(repeat > 0));
  el.repeat.setAttribute('aria-label', `Repeat: ${REPEAT[repeat]}`);
  el.repeat.title = `Repeat: ${REPEAT[repeat]}`;
  el.repeatBadge.hidden = repeat !== 1;

  // Seek bar
  const dur = +st.duration || 0;
  const canSeek = dur > 0 && st.can_seek !== 0 && st.can_seek !== '0' && !d.remote;
  el.seek.max = dur || 0;
  el.seek.disabled = !canSeek;
  el.seek.style.visibility = d.remote && !dur ? 'hidden' : '';
  el.duration.textContent = dur ? fmtTime(dur) : d.remote ? 'Live' : '0:00';

  // Volume (negative = muted)
  const rawVol = +st['mixer volume'] || 0;
  const muted = rawVol < 0 || st['mixer muting'] === 1;
  if (Date.now() > volumeHoldUntil) {
    el.volume.value = Math.abs(rawVol);
    el.volValue.textContent = Math.abs(rawVol);
    fill(el.volume);
  }
  setIcon(el.mute, muted ? 'mute' : 'volume');
  el.mute.setAttribute('aria-pressed', String(muted));
  el.mute.setAttribute('aria-label', muted ? 'Unmute' : 'Mute');
  el.mute.title = muted ? 'Unmute' : 'Mute';
  el.volume.classList.toggle('muted', muted);
  const fixedVolume = +st.digital_volume_control === 0;
  el.volume.disabled = fixedVolume;
  el.volume.title = fixedVolume ? 'Volume is fixed (digital volume control disabled)' : '';

  tick();
  renderMini(hasTrack ? { d, st, tr } : null);
  updateTitle(hasTrack && d.title ? `${playing ? '▶ ' : ''}${d.title}${d.artist ? ' – ' + d.artist : ''} · RMS` : null);
}

let ownsTitle = false;
/** While something is loaded, the browser tab shows the track; otherwise views set it. */
function updateTitle(t) {
  if (t) { if (document.title !== t) document.title = t; ownsTitle = true; }
  else if (ownsTitle) { document.title = 'RMS'; ownsTitle = false; }
}

function setLine(node, text, href) {
  node.replaceChildren(text ? (href ? h('a', { href }, text) : text) : '');
  node.hidden = !text;
}

function showEmpty(content) {
  el.empty.replaceChildren(content);
  el.empty.hidden = false;
  el.body.hidden = true;
}

function renderMini(info) {
  if (!info) { mini.root.hidden = true; document.body.classList.remove('has-mini'); return; }
  const { d, st, tr } = info;
  mini.root.hidden = false;
  document.body.classList.add('has-mini');
  mini.title.textContent = d.title || 'Unknown title';
  mini.artist.textContent = [d.artist, d.album].filter(Boolean).join(' · ');
  const src = artUrl(st, tr).replace(/size=600/, 'size=100');
  if (mini.img.dataset.src !== src) { mini.img.dataset.src = src; mini.img.src = src; }
  const playing = st.mode === 'play';
  setIcon(mini.play, playing ? 'pause' : 'play');
  mini.play.setAttribute('aria-label', playing ? 'Pause' : 'Play');
}

/** Runs 4x per second: interpolates elapsed time while playing. */
function tick() {
  const st = store.status;
  if (!st || document.hidden) return;
  const dur = +st.duration || 0;
  const t = elapsed();
  if (!seeking) {
    el.seek.value = t;
    el.elapsed.textContent = fmtTime(t);
    fill(el.seek);
    el.seek.setAttribute('aria-valuetext', `${fmtTime(t)} of ${fmtTime(dur)}`);
  }
  mini.progress.firstChild.style.width = dur ? `${Math.min(100, (t / dur) * 100)}%` : '0';
}

/** Paint the filled part of a range input via a CSS variable. */
function fill(input) {
  const max = +input.max || 0;
  input.style.setProperty('--pct', max ? `${((+input.value - +input.min) / (max - +input.min)) * 100}%` : '0%');
}

// ---------------------------------------------------------------- actions

function cycleShuffle() {
  const next = ((+store.status?.['playlist shuffle'] || 0) + 1) % 3;
  patchStatus({ 'playlist shuffle': next });
  ctx.cmd('playlist', 'shuffle', next).then(() => ctx.refreshSoon(300, { queue: true }));
  toast(`Shuffle ${SHUFFLE[next]}`);
}

function cycleRepeat() {
  const next = REPEAT_NEXT[+store.status?.['playlist repeat'] || 0];
  patchStatus({ 'playlist repeat': next });
  ctx.cmd('playlist', 'repeat', next);
  toast(`Repeat ${REPEAT[next]}`);
}

function toggleMute() {
  const v = +store.status?.['mixer volume'] || 0;
  patchStatus({ 'mixer volume': -v });
  ctx.cmd('mixer', 'muting', 'toggle');
}

async function addFavorite() {
  const st = store.status, tr = currentTrack();
  if (!tr?.url) { toast('Nothing to add to favorites', 'error'); return; }
  const d = describe(st, tr);
  const title = d.remote ? (tr.remote_title || d.album || d.title) : [d.title, d.artist].filter(Boolean).join(' – ');
  const r = await ctx.serverCmd('favorites', 'add', `url:${tr.url}`, `title:${title}`);
  if (r) toast(`Added “${title}” to favorites`);
}
