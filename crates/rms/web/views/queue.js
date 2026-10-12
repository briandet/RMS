// Play queue: highlight current, click to jump, remove, drag (or arrow keys) to reorder, clear,
// save as a playlist; each track's menu (⋯, right-click, long-press).

import { store, on, emit, currentPlayer, patchPlayer } from '../store.js';
import { h, icon, fmtTime, fmtDuration, plural, scrollParent, toast } from '../util.js';
import { sizedArtwork, createPlaylist, itemFor } from '../api.js';
import { withMenu, addToPlaylistDialog, askName } from './menu.js';

let ctx;
const el = {};
let currentIndex = -1;
let dragging = null;

export function init(root, _ctx) {
  ctx = _ctx;
  el.info = h('span', { class: 'queue-info muted' });
  el.clear = h('button', { class: 'btn btn-small btn-ghost', type: 'button', 'aria-label': 'Clear queue', onclick: clearQueue }, icon('trash'), 'Clear');
  el.save = h('button', { class: 'btn btn-small btn-ghost', type: 'button', 'aria-label': 'Save the queue as a playlist', title: 'Save as playlist', onclick: saveQueue }, icon('save'), 'Save');
  el.list = h('ol', { class: 'queue-list', 'aria-label': 'Queued tracks' });
  el.empty = h('p', { class: 'empty small' }, 'The queue is empty. Use ', h('strong', {}, 'Play'), ' or ', h('strong', {}, 'Add'), ' in the library to queue music.');
  root.append(h('div', { class: 'queue-head' }, h('h2', {}, 'Queue'), el.info, el.save, el.clear), el.list, el.empty);

  el.list.addEventListener('click', onClick);
  el.list.addEventListener('pointerdown', onPointerDown);
  el.list.addEventListener('keydown', onKeyDown);

  on('queue', render);
  on('status', updateCurrent);
  render();
}

/** Scroll the current track into view (used when the queue tab is opened). */
export function revealCurrent() {
  requestAnimationFrame(() => el.list.querySelector('.current')?.scrollIntoView({ block: 'center' }));
}

// ---------------------------------------------------------------- render

function render() {
  if (dragging) return; // don't yank the list out from under the pointer
  const q = store.queue;
  const hasPlayer = !!store.playerId;
  el.empty.hidden = q.length > 0 || !hasPlayer;
  el.clear.hidden = !q.length;
  el.save.hidden = !q.length;
  const total = q.reduce((s, t) => s + (t.duration || 0), 0);
  el.info.textContent = q.length ? `${plural(q.length, 'track')}${total ? ' · ' + fmtDuration(total) : ''}` : '';

  const frag = document.createDocumentFragment();
  q.forEach((t, i) => frag.append(item(t, i)));
  el.list.replaceChildren(frag);
  currentIndex = -1;
  updateCurrent();
}

function item(t, idx) {
  const title = t.title || t.url || 'Untitled';
  const sub = [t.artist, t.album].filter(Boolean).join(' · ');
  const art = sizedArtwork(t.artwork, 100);
  const thumb = h('span', { class: 'art thumb' }, icon(t.remote ? 'radio' : 'album', 'icon art-fallback'));
  if (art) {
    const img = h('img', { alt: '', loading: 'lazy', decoding: 'async', src: art, draggable: 'false' });
    img.addEventListener('load', () => thumb.classList.add('loaded'));
    img.addEventListener('error', () => img.remove());
    thumb.append(img);
  }
  const li = h('li', { class: 'q-item', dataset: { index: idx } },
    h('button', { class: 'icon-btn drag-handle', type: 'button', 'aria-label': `Move ${title}. Use up and down arrow keys to reorder.`, title: 'Drag to reorder' }, icon('drag')),
    h('button', { class: 'q-main', type: 'button', 'aria-label': `Play ${title}${t.artist ? ' by ' + t.artist : ''}` },
      thumb,
      h('span', { class: 'q-text' }, h('span', { class: 'q-title' }, title), sub && h('span', { class: 'q-sub' }, sub))),
    h('span', { class: 'q-dur' }, t.duration ? fmtTime(t.duration) : ''),
    h('button', { class: 'icon-btn q-remove', type: 'button', 'aria-label': `Remove ${title} from queue`, title: 'Remove' }, icon('close')));
  return withMenu(li, () => [title, queueEntries(t, +li.dataset.index, title)]);
}

/** A queued track's menu. */
function queueEntries(t, idx, title) {
  const item = t.id != null ? itemFor('track_id', t.id) : t.url ? itemFor('url', t.url) : null;
  return [
    { label: 'Play', icon: 'play', run: () => li(idx)?.querySelector('.q-main')?.click() },
    { label: 'Remove from queue', icon: 'close', run: () => li(idx)?.querySelector('.q-remove')?.click() },
    item && { label: 'Add to playlist…', icon: 'playlist', run: () => addToPlaylistDialog(item, title) },
    t.id != null && { label: 'Start a mix', icon: 'mix', run: () => ctx.createMix('track_id', t.id, title) },
    (t.album_id != null || t.artist_id != null) && 'sep',
    t.album_id != null && { label: 'Go to album', icon: 'album', run: () => { location.hash = `#/library/album/${encodeURIComponent(t.album_id)}?name=${encodeURIComponent(t.album || '')}`; } },
    t.artist_id != null && { label: 'Go to artist', icon: 'person', run: () => { location.hash = `#/library/artist/${encodeURIComponent(t.artist_id)}?name=${encodeURIComponent(t.artist || '')}`; } },
  ].filter(Boolean);
}

function li(idx) {
  return el.list.querySelector(`li[data-index="${idx}"]`);
}

async function saveQueue() {
  const name = await askName('Save the queue as a playlist', '', 'Save');
  if (!name || !store.playerId) return;
  try {
    await createPlaylist({ name, from_player: store.playerId });
    toast(`Saved the queue as ${name}`);
    ctx.invalidateViews?.();
  } catch (e) {
    toast(`Could not save the playlist: ${e.message}`, 'error');
  }
}

function updateCurrent() {
  const idx = currentPlayer()?.queue.index ?? -1;
  if (idx === currentIndex) return;
  el.list.querySelector('.current')?.classList.remove('current');
  el.list.querySelector('[aria-current]')?.removeAttribute('aria-current');
  currentIndex = idx;
  const li = el.list.querySelector(`li[data-index="${idx}"]`);
  if (!li) return;
  li.classList.add('current');
  li.setAttribute('aria-current', 'true');
  if (li.offsetParent !== null && !dragging) {
    const sp = scrollParent(el.list);
    const r = li.getBoundingClientRect(), pr = sp.getBoundingClientRect?.() || { top: 0, bottom: innerHeight };
    if (r.top < pr.top || r.bottom > pr.bottom) li.scrollIntoView({ block: 'nearest' });
  }
}

// ---------------------------------------------------------------- actions

function onClick(e) {
  const li = e.target.closest('li.q-item');
  if (!li) return;
  const idx = +li.dataset.index;
  if (e.target.closest('.q-remove')) {
    // Optimistic removal; indices after it shift down by one.
    store.queue = store.queue.filter((t, i) => i !== idx);
    const p = currentPlayer();
    if (p && p.queue.index > idx) p.queue.index -= 1;
    emit('queue', store.queue);
    ctx.send({ command: 'remove', index: idx }).then((r) => { if (!r) ctx.refreshQueue(); });
  } else if (e.target.closest('.q-main')) {
    const p = currentPlayer();
    if (p) patchPlayer({ queue: { ...p.queue, index: idx }, elapsed: 0 });
    ctx.send({ command: 'jump', index: idx });
  }
}

function clearQueue() {
  if (!store.queue.length) return;
  if (!confirm(`Remove all ${store.queue.length} tracks from the queue?`)) return;
  store.queue = [];
  emit('queue', store.queue);
  ctx.send({ command: 'clear' }).then((r) => { if (!r) ctx.refreshQueue(); });
}

/** Reorder locally (optimistic), then tell the server. */
function move(from, to) {
  if (from === to || to < 0 || to >= store.queue.length) return;
  const q = store.queue.slice();
  const [t] = q.splice(from, 1);
  q.splice(to, 0, t);
  // Keep the "current" highlight on the same track.
  const p = currentPlayer();
  const cur = p?.queue.index;
  if (Number.isFinite(cur)) {
    let n = cur;
    if (cur === from) n = to;
    else if (from < cur && to >= cur) n = cur - 1;
    else if (from > cur && to <= cur) n = cur + 1;
    p.queue.index = n;
  }
  store.queue = q;
  emit('queue', q);
  ctx.send({ command: 'move', from, to }).then((r) => { if (!r) ctx.refreshQueue(); });
}

function onKeyDown(e) {
  const handle = e.target.closest('.drag-handle');
  if (!handle || (e.key !== 'ArrowUp' && e.key !== 'ArrowDown')) return;
  e.preventDefault();
  const from = +handle.closest('li').dataset.index;
  const to = from + (e.key === 'ArrowUp' ? -1 : 1);
  if (to < 0 || to >= store.queue.length) return;
  move(from, to);
  el.list.querySelector(`li[data-index="${to}"] .drag-handle`)?.focus();
}

// Pointer-based drag (works for mouse, pen and touch, unlike HTML5 DnD).
function onPointerDown(e) {
  const handle = e.target.closest('.drag-handle');
  if (!handle || e.button !== 0) return;
  e.preventDefault();
  const li = handle.closest('li');
  const items = [...el.list.children];
  const from = items.indexOf(li);
  const scroller = scrollParent(el.list);
  const scrollerRect = () => (scroller === document.scrollingElement || scroller === document.documentElement
    ? { top: 0, bottom: innerHeight } : scroller.getBoundingClientRect());
  const startY = e.clientY, startScroll = scroller.scrollTop;
  let y = startY, slot = from, raf = 0;
  dragging = { from };
  try { handle.setPointerCapture(e.pointerId); } catch { /* ignore */ }
  li.classList.add('dragging');
  el.list.classList.add('is-dragging');

  const update = () => {
    li.style.transform = `translateY(${y - startY + (scroller.scrollTop - startScroll)}px)`;
    slot = items.length;
    for (let i = 0; i < items.length; i++) {
      if (items[i] === li) continue;
      const r = items[i].getBoundingClientRect();
      if (y < r.top + r.height / 2) { slot = i; break; }
    }
    for (const x of items) x.classList.remove('drop-before', 'drop-after');
    const noop = slot === from || slot === from + 1;
    if (!noop) {
      if (slot < items.length) items[slot].classList.add('drop-before');
      else items[items.length - 1].classList.add('drop-after');
    }
  };
  const autoScroll = () => {
    const r = scrollerRect();
    const edge = 56;
    let v = 0;
    if (y < r.top + edge) v = -Math.ceil((r.top + edge - y) / 4);
    else if (y > r.bottom - edge) v = Math.ceil((y - (r.bottom - edge)) / 4);
    if (v) { scroller.scrollTop += v; update(); }
    raf = requestAnimationFrame(autoScroll);
  };
  const onMove = (ev) => { y = ev.clientY; update(); };
  const onUp = (ev) => {
    cancelAnimationFrame(raf);
    handle.removeEventListener('pointermove', onMove);
    handle.removeEventListener('pointerup', onUp);
    handle.removeEventListener('pointercancel', onUp);
    li.style.transform = '';
    li.classList.remove('dragging');
    el.list.classList.remove('is-dragging');
    for (const x of items) x.classList.remove('drop-before', 'drop-after');
    dragging = null;
    if (ev.type === 'pointercancel') { render(); return; }
    const to = slot > from ? slot - 1 : slot;
    if (to !== from) move(from, to); else render();
  };
  handle.addEventListener('pointermove', onMove);
  handle.addEventListener('pointerup', onUp);
  handle.addEventListener('pointercancel', onUp);
  raf = requestAnimationFrame(autoScroll);
}
