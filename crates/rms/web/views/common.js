// Building blocks shared by library, search and settings views.

import { h, icon, fmtTime } from '../util.js';
import { itemArt } from '../api.js';

export const enc = encodeURIComponent;

// ---------------------------------------------------------------- lazy images

let imgObserver = null;
let imgObserverRoot = null;

/** <div class="art"><img></div> whose src is set when scrolled near the viewport. */
export function lazyArt(src, { cls = '', alt = '', fallback = 'album' } = {}) {
  const wrap = h('div', { class: `art ${cls}`.trim() }, icon(fallback, 'icon art-fallback'));
  if (!src) return wrap;
  const img = h('img', { alt, decoding: 'async', draggable: 'false' });
  img.dataset.src = src;
  img.addEventListener('load', () => wrap.classList.add('loaded'));
  img.addEventListener('error', () => img.remove());
  wrap.append(img);
  observeImg(img);
  return wrap;
}

function observeImg(img) {
  const root = document.getElementById('main');
  if (!('IntersectionObserver' in window)) { img.src = img.dataset.src; return; }
  if (!imgObserver || imgObserverRoot !== root) {
    imgObserverRoot = root;
    imgObserver = new IntersectionObserver((entries) => {
      for (const e of entries) {
        if (!e.isIntersecting) continue;
        const el = e.target;
        el.src = el.dataset.src;
        imgObserver.unobserve(el);
      }
    }, { root, rootMargin: '400px 0px' });
  }
  imgObserver.observe(img);
}

// ---------------------------------------------------------------- infinite paging

/**
 * Load items page by page (start/count) as a sentinel scrolls into view.
 * fetchPage(start, count) -> {items, total}; renderItem(item, index) -> Node|Node[]
 */
export function pager(v, container, fetchPage, renderItem, { pageSize = 100, empty = 'Nothing here.', onFirst } = {}) {
  let start = 0, total = Infinity, loading = false, done = false;
  const sentinel = h('div', { class: 'sentinel', 'aria-hidden': 'true' }, h('span', { class: 'spinner' }));
  container.after(sentinel);

  const io = new IntersectionObserver((entries) => {
    if (entries.some((e) => e.isIntersecting)) load();
  }, { root: v.scrollRoot, rootMargin: '0px 0px 800px 0px' });

  async function load() {
    if (loading || done || !v.alive) return;
    loading = true;
    try {
      const { items, total: t } = await fetchPage(start, pageSize);
      if (!v.alive) return;
      total = Number.isFinite(+t) ? +t : total;
      const frag = document.createDocumentFragment();
      items.forEach((it, i) => {
        const n = renderItem(it, start + i);
        if (Array.isArray(n)) frag.append(...n); else if (n) frag.append(n);
      });
      container.append(frag);
      if (start === 0) onFirst?.(total, items);
      start += items.length;
      if (!items.length || start >= total) finish();
      else { io.unobserve(sentinel); io.observe(sentinel); } // re-check: still visible -> load more
    } catch (e) {
      if (!v.alive) return;
      finish();
      container.after(errorBox(e, () => v.ctx.reload()));
    } finally {
      loading = false;
    }
  }

  function finish() {
    done = true;
    io.disconnect();
    sentinel.remove();
    if (start === 0 && !container.children.length) container.replaceWith(emptyBox(empty));
  }

  io.observe(sentinel);
  v.cleanup.push(() => io.disconnect());
}

// ---------------------------------------------------------------- states

export function emptyBox(content) {
  return h('div', { class: 'empty' }, content);
}

export function errorBox(e, retry) {
  return h('div', { class: 'empty error', role: 'alert' },
    h('p', {}, `Could not load: ${e?.message || e}`),
    retry && h('button', { class: 'btn', type: 'button', onclick: retry }, icon('refresh'), 'Retry'));
}

export function loadingBox() {
  return h('div', { class: 'loading', role: 'status', 'aria-label': 'Loading' }, h('span', { class: 'spinner' }));
}

/** Help text for when no players are connected. */
export function connectHelp() {
  const host = location.hostname || 'this-server';
  return h('div', { class: 'help' },
    h('p', {}, 'No players are connected to this server yet. Any Squeezebox-compatible player can be pointed at it:'),
    h('ul', {},
      h('li', {}, h('strong', {}, 'squeezelite: '), h('code', {}, `squeezelite -s ${host}`)),
      h('li', {}, h('strong', {}, 'piCorePlayer: '), 'Squeezelite settings → “LMS IP” → ', h('code', {}, host)),
      h('li', {}, h('strong', {}, 'Squeezebox / Touch / Radio / Boom: '), 'Settings → Advanced → Networking → Remote Library → add ', h('code', {}, host)),
      h('li', {}, h('strong', {}, 'WiiM and other LMS-capable streamers: '), 'enable the “Squeezelite / LMS” source and choose this server (', h('code', {}, host), ')'),
    ),
    h('p', { class: 'muted' }, 'Players connect on TCP/UDP port 3483 and are discovered automatically on the local network; make sure that port is reachable.'));
}

// ---------------------------------------------------------------- actions

/** Play / Play next / Add buttons for something playlistcontrol understands. */
export function actionButtons(ctx, key, value, label, { run, compact = false } = {}) {
  const go = (action) => (e) => {
    e.preventDefault(); e.stopPropagation();
    if (run) run(action); else ctx.playItem(action, key, value, label);
  };
  return h('div', { class: `actions${compact ? ' compact' : ''}` },
    h('button', { class: 'icon-btn act-play', type: 'button', 'aria-label': `Play ${label}`, title: 'Play now', onclick: go('load') }, icon('play')),
    h('button', { class: 'icon-btn', type: 'button', 'aria-label': `Play ${label} next`, title: 'Play next', onclick: go('insert') }, icon('playnext')),
    h('button', { class: 'icon-btn', type: 'button', 'aria-label': `Add ${label} to queue`, title: 'Add to queue', onclick: go('add') }, icon('add')));
}

// ---------------------------------------------------------------- rows & cards

/**
 * A list row. Either `href` (navigates) or `onActivate` (button) for the main area.
 */
export function row({ href, onActivate, title, sub, meta, art, iconName = 'note', actions, cls = '', label }) {
  const lead = art !== undefined ? lazyArt(art, { cls: 'thumb', fallback: iconName }) : h('span', { class: 'row-icon' }, icon(iconName));
  const text = h('span', { class: 'row-text' },
    h('span', { class: 'row-title' }, title || '—'),
    sub && h('span', { class: 'row-sub' }, sub));
  const mainAttrs = { class: 'row-main', 'aria-label': label };
  const main = href
    ? h('a', { ...mainAttrs, href }, lead, text, h('span', { class: 'chev' }, icon('chevron')))
    : h('button', { ...mainAttrs, type: 'button', onclick: onActivate }, lead, text);
  return h('li', { class: `row ${cls}`.trim() }, main, meta != null && h('span', { class: 'row-meta' }, meta), actions);
}

export function trackRow(ctx, t, { onPlay, showArtist = true, showAlbum = false, number, cls = '' } = {}) {
  const title = t.title || t.track || t.filename || 'Untitled';
  const subParts = [];
  if (showArtist && t.artist) subParts.push(t.artist);
  if (showAlbum && t.album) subParts.push(t.album);
  const id = t.id ?? t.track_id;
  const numEl = number != null ? h('span', { class: 'track-no' }, number) : null;
  const r = row({
    title, sub: subParts.join(' · ') || null,
    meta: +t.duration ? fmtTime(t.duration) : '',
    iconName: 'note',
    label: `Play ${title}`,
    onActivate: onPlay || (() => ctx.playItem('load', 'track_id', id, title)),
    actions: id != null ? actionButtons(ctx, 'track_id', id, title, { compact: true }) : null,
    cls: `track ${cls}`,
  });
  if (numEl) r.querySelector('.row-main').prepend(numEl);
  return r;
}

export function albumCard(ctx, a) {
  const id = a.id ?? a.album_id;
  const title = a.album || 'Unknown album';
  const sub = [a.artist, +a.year ? a.year : null].filter(Boolean).join(' · ');
  const href = `#/library/album/${enc(id)}?name=${enc(title)}`;
  return h('div', { class: 'card', role: 'listitem' },
    h('a', { class: 'card-link', href, 'aria-label': `${title}${a.artist ? ' by ' + a.artist : ''}` },
      lazyArt(itemArt(a, 300), { fallback: 'album' }),
      h('span', { class: 'card-title' }, title),
      h('span', { class: 'card-sub' }, sub || ' ')),
    actionButtons(ctx, 'album_id', id, title));
}

export function albumGrid() {
  return h('div', { class: 'grid', role: 'list' });
}

export function list(label) {
  return h('ul', { class: 'list', 'aria-label': label });
}

/** Header block for drill-down pages (title + subtitle + action buttons). */
export function pageHeader({ title, sub, art, actions, iconName }) {
  return h('header', { class: 'page-head' },
    art !== undefined && lazyArt(art, { cls: 'page-art', fallback: iconName || 'album' }),
    h('div', { class: 'page-head-text' },
      h('h1', {}, title),
      sub && h('div', { class: 'page-sub' }, sub),
      actions && h('div', { class: 'page-actions' }, actions)));
}

/** Larger labelled action buttons for page headers. */
export function bigActions(ctx, key, value, label, { run } = {}) {
  const go = (action) => () => (run ? run(action) : ctx.playItem(action, key, value, label));
  return [
    h('button', { class: 'btn btn-primary', type: 'button', onclick: go('load'), 'aria-label': `Play ${label}` }, icon('play'), 'Play'),
    h('button', { class: 'btn', type: 'button', onclick: go('insert'), 'aria-label': `Play ${label} next` }, icon('playnext'), 'Play next'),
    h('button', { class: 'btn', type: 'button', onclick: go('add'), 'aria-label': `Add ${label} to queue` }, icon('add'), 'Add'),
  ];
}
