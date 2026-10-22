// Now Playing panel (side column on desktop, own tab on mobile) and the mobile mini player.

import { store, on, elapsed, currentTrack, currentPlayer, patchPlayer } from '../store.js';
import { h, icon, setIcon, fmtTime, throttle, toast, isSpotify, spotifyBadge } from '../util.js';
import { sizedArtwork, currentArtUrl, addFavorite as addFavoriteApi } from '../api.js';
import { connectHelp, enc } from './common.js';
import { spectrumBars } from '../spectrum.js';
import { openVisualiser, visualiserFor } from './visualiser.js';

const SHUFFLE = { off: 'off', songs: 'by song', albums: 'by album' };
const SHUFFLE_NEXT = { off: 'songs', songs: 'albums', albums: 'off' };
const REPEAT = { off: 'off', one: 'one track', all: 'whole queue' };
const REPEAT_NEXT = { off: 'all', all: 'one', one: 'off' };

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
  on('browser-player', render);
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
  el.source = spotifyBadge('np-source');
  el.source.hidden = true;
  el.fav = btn('heart', 'Add to favorites', addFavorite, 'np-fav');
  // while this browser plays along (see visualiser.js)
  el.viz = btn('viz', 'Visualiser', openVisualiser, 'np-viz');
  el.viz.hidden = true;

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
    patchPlayer({ elapsed: t });
    ctx.send({ command: 'seek', position: t });
  });

  el.shuffle = btn('shuffle', 'Shuffle', cycleShuffle, 'toggle');
  el.prev = btn('prev', 'Previous track', () => ctx.send({ command: 'previous' }));
  el.play = btn('play', 'Play', () => ctx.togglePlay(), 'play-btn');
  el.next = btn('next', 'Next track', () => ctx.send({ command: 'next' }));
  el.repeat = btn('repeat', 'Repeat', cycleRepeat, 'toggle');
  el.repeatBadge = h('span', { class: 'badge', hidden: true }, '1');
  el.repeat.append(el.repeatBadge);

  el.mute = btn('volume', 'Mute', toggleMute);
  el.volume = h('input', { type: 'range', class: 'range volume', min: 0, max: 100, step: 1, value: 0, 'aria-label': 'Volume' });
  el.volValue = h('span', { class: 'vol-value', 'aria-hidden': 'true' }, '0');
  const sendVolume = throttle((v) => ctx.send({ command: 'volume', value: v }), 150);
  el.volume.addEventListener('input', () => {
    volumeHoldUntil = Date.now() + 2000;
    const v = +el.volume.value;
    el.volValue.textContent = v;
    fill(el.volume);
    const p = currentPlayer();
    if (p) p.volume = v;
    sendVolume(v);
  });
  el.volume.addEventListener('change', () => { volumeHoldUntil = Date.now() + 1500; });

  el.body = h('div', { class: 'np-body' },
    el.notice,
    el.art,
    h('div', { class: 'np-meta' },
      h('div', { class: 'np-text' }, el.title, el.artist, el.album, el.source),
      spectrumBars(() => store.playerId, 'np-bars'),
      el.viz,
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
  mini.next = h('button', { class: 'icon-btn', type: 'button', 'aria-label': 'Next track', onclick: () => ctx.send({ command: 'next' }) }, icon('next'));
  root.append(mini.progress,
    h('a', { class: 'mini-main', href: '#/now', 'aria-label': 'Open now playing' },
      h('span', { class: 'art mini-art' }, icon('album', 'icon art-fallback'), mini.img),
      h('span', { class: 'mini-text' }, mini.title, mini.artist)),
    mini.play, mini.next);
}

// ---------------------------------------------------------------- render

/** The Spotify menu level of an album or artist (`album:<id>`). */
function spotifyLink(id, name) {
  return `#/library/spotify/${enc(id)}?name=${enc(name || '')}`;
}

/** Title / artist / album lines; a radio station shows next to the album. */
function describe(tr) {
  if (!tr) return { title: '', artist: '', album: '', remote: false };
  let album = tr.album || '';
  if (tr.station && tr.station !== tr.title) album = album ? `${album} · ${tr.station}` : tr.station;
  return { title: tr.title || '', artist: tr.artist || '', album, remote: tr.remote };
}

function artUrl(p, tr, size = 600) {
  if (tr?.artwork) return sizedArtwork(tr.artwork, size);
  return currentArtUrl(p.id, `${tr?.id ?? ''}-${tr?.title || ''}`, size);
}

function render() {
  const st = currentPlayer();
  if (!store.players.length) {
    showEmpty(store.server ? connectHelp() : h('p', { class: 'muted' }, 'Connecting to server…'));
    renderMini(null);
    return;
  }
  if (!st) {
    showEmpty(h('p', { class: 'muted' }, store.playerId ? 'Loading player…' : 'Select a player.'));
    renderMini(null);
    return;
  }
  el.empty.hidden = true;
  el.body.hidden = false;

  const tr = currentTrack();
  const hasTrack = !!tr;
  const d = describe(tr);
  el.viz.hidden = !(hasTrack && visualiserFor(st));

  // Notices: player off / disconnected
  let notice = null;
  if (!st.connected) notice = 'Player is disconnected.';
  else if (!st.power) notice = h('span', {}, 'Player is off. ', h('button', { class: 'btn btn-small', type: 'button', onclick: () => ctx.send({ command: 'power', on: true }) }, 'Turn on'));
  el.notice.replaceChildren(notice || '');
  el.notice.hidden = !notice;

  if (hasTrack) {
    el.title.textContent = d.title || 'Unknown title';
    setLine(el.artist, d.artist, tr?.artist_id != null && !d.remote ? `#/library/artist/${enc(tr.artist_id)}?name=${enc(d.artist)}`
      : tr?.spotify_artist ? spotifyLink(tr.spotify_artist, tr.artist) : null);
    setLine(el.album, d.album, tr?.album_id != null && !d.remote ? `#/library/album/${enc(tr.album_id)}?name=${enc(d.album)}`
      : tr?.spotify_album ? spotifyLink(tr.spotify_album, tr.album) : null);
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
  el.source.hidden = !isSpotify(tr?.url);
  el.fav.hidden = !(tr?.url || d.remote);

  // Transport
  const playing = st.mode === 'play';
  setIcon(el.play, playing ? 'pause' : 'play');
  el.play.setAttribute('aria-label', playing ? 'Pause' : 'Play');
  el.play.title = playing ? 'Pause (space)' : 'Play (space)';
  for (const b of [el.prev, el.next, el.play]) b.disabled = !hasTrack;

  const shuffle = st.shuffle;
  el.shuffle.setAttribute('aria-pressed', String(shuffle !== 'off'));
  el.shuffle.setAttribute('aria-label', `Shuffle: ${SHUFFLE[shuffle]}`);
  el.shuffle.title = `Shuffle: ${SHUFFLE[shuffle]}`;
  el.shuffle.dataset.mode = shuffle;

  const repeat = st.repeat;
  el.repeat.setAttribute('aria-pressed', String(repeat !== 'off'));
  el.repeat.setAttribute('aria-label', `Repeat: ${REPEAT[repeat]}`);
  el.repeat.title = `Repeat: ${REPEAT[repeat]}`;
  el.repeatBadge.hidden = repeat !== 'one';

  // Seek bar
  const dur = st.duration || 0;
  const canSeek = dur > 0 && st.can_seek && !d.remote;
  el.seek.max = dur || 0;
  el.seek.disabled = !canSeek;
  el.seek.style.visibility = d.remote && !dur ? 'hidden' : '';
  el.duration.textContent = dur ? fmtTime(dur) : d.remote ? 'Live' : '0:00';

  // Volume
  const muted = st.muted;
  if (Date.now() > volumeHoldUntil) {
    el.volume.value = st.volume;
    el.volValue.textContent = st.volume;
    fill(el.volume);
  }
  setIcon(el.mute, muted ? 'mute' : 'volume');
  el.mute.setAttribute('aria-pressed', String(muted));
  el.mute.setAttribute('aria-label', muted ? 'Unmute' : 'Mute');
  el.mute.title = muted ? 'Unmute' : 'Mute';
  el.volume.classList.toggle('muted', muted);
  const fixedVolume = st.volume_fixed;
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
  const src = artUrl(st, tr, 100);
  if (mini.img.dataset.src !== src) { mini.img.dataset.src = src; mini.img.src = src; }
  const playing = st.mode === 'play';
  setIcon(mini.play, playing ? 'pause' : 'play');
  mini.play.setAttribute('aria-label', playing ? 'Pause' : 'Play');
}

/** Runs 4x per second: interpolates elapsed time while playing. */
function tick() {
  const st = currentPlayer();
  if (!st || document.hidden) return;
  const dur = st.duration || 0;
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
  const next = SHUFFLE_NEXT[currentPlayer()?.shuffle || 'off'];
  patchPlayer({ shuffle: next });
  ctx.send({ command: 'shuffle', mode: next });
  toast(`Shuffle ${SHUFFLE[next]}`);
}

function cycleRepeat() {
  const next = REPEAT_NEXT[currentPlayer()?.repeat || 'off'];
  patchPlayer({ repeat: next });
  ctx.send({ command: 'repeat', mode: next });
  toast(`Repeat ${REPEAT[next]}`);
}

function toggleMute() {
  const p = currentPlayer();
  if (!p) return;
  patchPlayer({ muted: !p.muted });
  ctx.send({ command: 'mute', muted: p.muted });
}

async function addFavorite() {
  const tr = currentTrack();
  if (!tr?.url) { toast('Nothing to add to favorites', 'error'); return; }
  const d = describe(tr);
  const title = d.remote ? (tr.station || tr.album || d.title) : [d.title, d.artist].filter(Boolean).join(' – ');
  try {
    await addFavoriteApi(tr.url, title);
    toast(`Added “${title}” to favorites`);
  } catch (e) {
    toast(`Could not add to favorites: ${e.message}`, 'error');
  }
}
