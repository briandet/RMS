// Context menus: right-click, long-press or the ⋯ button on a track, album, artist, playlist or
// queue row opens a menu of what can be done with it (play, queue, mix, add to a playlist, go
// to its album or artist, remove).  On a phone the menu is a sheet at the bottom.

import { h, icon, toast } from '../util.js';
import { itemFor, getPlaylists, createPlaylist, addToPlaylist } from '../api.js';

const enc = encodeURIComponent;

let open = null;

/** Close the open menu (if any). */
export function closeMenu() {
  if (!open) return;
  const { el, restore, onKey, onAway, onClose } = open;
  open = null;
  el.remove();
  document.removeEventListener('keydown', onKey, true);
  document.removeEventListener('pointerdown', onAway, true);
  window.removeEventListener('resize', closeMenu);
  document.getElementById('main')?.removeEventListener('scroll', closeMenu);
  restore?.focus?.();
  onClose?.();
}

/**
 * Show a menu.  `at`: {x, y} (pointer) or an element to open under.  `entries`: [{label,
 * icon, run, danger, checked, detail}], 'sep' for a separator, or {heading, icon} to start a
 * group of entries under a heading.  `checked` (true or false) makes the entries a choice of
 * one, the checked one highlighted.  `detail` (text or an element) goes at the end.  `align: 'start'` lines the menu up with the element's left
 * edge instead of its right.  `onClose` runs when it closes.
 */
export function openMenu(at, title, entries, { align = 'end', onClose } = {}) {
  closeMenu();
  const sheet = matchMedia('(max-width: 700px)').matches;
  const buttons = [];
  let first = null;
  const el = h('div', { class: `menu${sheet ? ' sheet' : ''}`, role: 'menu', 'aria-label': title },
    sheet && h('div', { class: 'menu-title' }, title));
  let parent = el;
  for (const e of entries.filter(Boolean)) {
    if (e === 'sep') {
      el.append(h('div', { class: 'menu-sep', role: 'separator' }));
      parent = el;
      continue;
    }
    if (e.heading) {
      const id = `menu-h-${Math.random().toString(36).slice(2)}`;
      parent = h('div', { class: 'menu-group', role: 'group', 'aria-labelledby': id },
        h('div', { class: 'menu-heading', id }, e.icon && icon(e.icon), e.heading));
      el.append(parent);
      continue;
    }
    const choice = typeof e.checked === 'boolean';
    const b = h('button', {
      class: `menu-item${e.danger ? ' danger' : ''}${e.checked ? ' checked' : ''}`, type: 'button',
      role: choice ? 'menuitemradio' : 'menuitem', 'aria-checked': choice ? String(e.checked) : null, tabindex: '-1',
      onclick: (ev) => { ev.stopPropagation(); closeMenu(); e.run(); },
    }, e.icon && icon(e.icon), h('span', { class: 'menu-label' }, e.label),
    e.detail && h('span', { class: 'menu-detail' }, e.detail));
    buttons.push(b);
    if (e.checked) first = b;
    parent.append(b);
  }
  const restore = document.activeElement;
  document.body.append(el);
  if (!sheet) {
    const r = at instanceof Element ? at.getBoundingClientRect() : { left: at.x, right: at.x, top: at.y, bottom: at.y };
    const w = el.offsetWidth, ht = el.offsetHeight;
    let x = at instanceof Element && align !== 'start' ? r.right - w : r.left;
    let y = r.bottom + 4;
    if (y + ht > innerHeight - 8) y = Math.max(8, r.top - ht - 4);
    x = Math.min(Math.max(8, x), innerWidth - w - 8);
    el.style.left = `${x}px`;
    el.style.top = `${y}px`;
  }
  const focus = (i) => buttons[(i + buttons.length) % buttons.length]?.focus();
  const onKey = (e) => {
    const i = buttons.indexOf(document.activeElement);
    if (e.key === 'Escape') { e.preventDefault(); closeMenu(); }
    else if (e.key === 'ArrowDown') { e.preventDefault(); focus(i + 1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); focus(i < 0 ? -1 : i - 1); }
    else if (e.key === 'Home') { e.preventDefault(); focus(0); }
    else if (e.key === 'End') { e.preventDefault(); focus(-1); }
    else if (e.key === 'Tab') closeMenu();
  };
  const onAway = (e) => { if (!el.contains(e.target)) closeMenu(); };
  open = { el, restore, onKey, onAway, onClose };
  document.addEventListener('keydown', onKey, true);
  setTimeout(() => document.addEventListener('pointerdown', onAway, true));
  window.addEventListener('resize', closeMenu);
  document.getElementById('main')?.addEventListener('scroll', closeMenu, { passive: true });
  if (first) first.focus(); else focus(0);
}

/**
 * Give `el` a menu: right-click, long-press (touch), and a ⋯ button (appended to `slot`, or
 * to `el`).  `build()` returns [title, entries] when the menu opens.
 */
export function withMenu(el, build, slot = el) {
  const show = (at) => { const [title, entries] = build(); openMenu(at, title, entries); };
  const more = h('button', { class: 'icon-btn more-btn', type: 'button', 'aria-haspopup': 'menu', title: 'More', 'aria-label': `More options for ${build()[0]}`,
    onclick: (e) => { e.preventDefault(); e.stopPropagation(); show(more); } }, icon('more'));
  slot.append(more);
  el.addEventListener('contextmenu', (e) => {
    if (e.target.closest('input, textarea')) return;
    e.preventDefault();
    show({ x: e.clientX, y: e.clientY });
  });
  // long-press on touch screens
  let timer = null, start = null;
  el.addEventListener('pointerdown', (e) => {
    if (e.pointerType !== 'touch') return;
    start = { x: e.clientX, y: e.clientY };
    timer = setTimeout(() => {
      timer = null;
      navigator.vibrate?.(10);
      show({ x: start.x, y: start.y });
      // the click that ends the press doesn't also play
      el.addEventListener('click', (c) => { c.preventDefault(); c.stopPropagation(); }, { capture: true, once: true });
    }, 500);
  });
  const cancel = (e) => {
    if (!timer) return;
    if (e.type === 'pointermove' && Math.hypot(e.clientX - start.x, e.clientY - start.y) < 10) return;
    clearTimeout(timer);
    timer = null;
  };
  for (const ev of ['pointerup', 'pointercancel', 'pointermove']) el.addEventListener(ev, cancel);
  return el;
}

const MIXABLE = new Set(['track_id', 'album_id', 'artist_id', 'genre_id', 'year']);

/**
 * The usual entries for a library thing: play, play next, add, mix, add to a playlist, and
 * go to its album or artist.  `extra` entries go at the end.
 */
export function itemEntries(ctx, key, value, label, { album, artist, extra = [] } = {}) {
  return [
    { label: 'Play now', icon: 'play', run: () => ctx.playItem('load', key, value, label) },
    { label: 'Play next', icon: 'playnext', run: () => ctx.playItem('insert', key, value, label) },
    { label: 'Add to queue', icon: 'add', run: () => ctx.playItem('add', key, value, label) },
    MIXABLE.has(key) && { label: 'Start a mix', icon: 'mix', run: () => ctx.createMix(key, value, label) },
    itemFor(key, value) && { label: 'Add to playlist…', icon: 'playlist', run: () => addToPlaylistDialog(itemFor(key, value), label) },
    (album || artist) && 'sep',
    album && { label: 'Go to album', icon: 'album', run: () => { location.hash = `#/library/album/${enc(album.id)}?name=${enc(album.name || '')}`; } },
    artist && { label: 'Go to artist', icon: 'person', run: () => { location.hash = `#/library/artist/${enc(artist.id)}?name=${enc(artist.name || '')}`; } },
    extra.length && 'sep',
    ...extra,
  ].filter(Boolean);
}

/** Entries for a native Track (library track or not). */
export function trackEntries(ctx, t, extra = []) {
  const album = t.album_id != null ? { id: t.album_id, name: t.album } : null;
  const artist = t.artist_id != null ? { id: t.artist_id, name: t.artist } : null;
  if (t.id != null) return itemEntries(ctx, 'track_id', t.id, t.title || 'track', { album, artist, extra });
  return [
    t.url && { label: 'Play now', icon: 'play', run: () => ctx.playItem('load', 'url', t.url, t.title) },
    t.url && { label: 'Add to queue', icon: 'add', run: () => ctx.playItem('add', 'url', t.url, t.title) },
    ...extra,
  ].filter(Boolean);
}

// ---------------------------------------------------------------- add to a playlist

/** Ask which playlist (or a new one) to add `item` to, and add it. */
export async function addToPlaylistDialog(item, label) {
  let playlists = [];
  try {
    playlists = (await getPlaylists({ limit: 500 })).items.filter((p) => p.editable);
  } catch (e) {
    toast(`Could not load the playlists: ${e.message}`, 'error');
    return;
  }
  const name = h('input', { type: 'text', placeholder: 'New playlist name', 'aria-label': 'New playlist name', autocomplete: 'off' });
  const dlg = h('dialog', { class: 'dialog', 'aria-label': `Add ${label} to a playlist` },
    h('h2', {}, 'Add to playlist'),
    h('p', { class: 'muted small' }, label),
    playlists.length ? h('ul', { class: 'pick-list' }, ...playlists.map((p) => h('li', {},
      h('button', { class: 'pick', type: 'button', onclick: () => done(p) }, icon('playlist'), p.name)))) : null,
    h('form', { class: 'inline-row', onsubmit: (e) => { e.preventDefault(); if (name.value.trim()) done(null, name.value.trim()); } },
      name, h('button', { class: 'btn btn-primary', type: 'submit' }, 'Create')),
    h('div', { class: 'dialog-actions' }, h('button', { class: 'btn btn-ghost', type: 'button', onclick: () => dlg.close() }, 'Cancel')));
  dlg.addEventListener('close', () => dlg.remove());
  document.body.append(dlg);
  dlg.showModal();
  if (!playlists.length) name.focus();

  async function done(p, newName) {
    dlg.close();
    try {
      if (p) {
        const r = await addToPlaylist(p.id, item);
        toast(`Added ${r.count === 1 ? label : `${r.count} tracks`} to ${p.name}`);
      } else {
        await createPlaylist({ name: newName, item });
        toast(`Made the playlist ${newName}`);
      }
    } catch (e) {
      toast(`Could not add to the playlist: ${e.message}`, 'error');
    }
  }
}

/** Ask for a name (prefilled with `value`); resolves to the trimmed name, or null. */
export function askName(title, value = '', action = 'Save') {
  return new Promise((resolve) => {
    const input = h('input', { type: 'text', value, 'aria-label': 'Name', autocomplete: 'off' });
    let result = null;
    const dlg = h('dialog', { class: 'dialog', 'aria-label': title },
      h('h2', {}, title),
      h('form', { class: 'inline-row', onsubmit: (e) => { e.preventDefault(); result = input.value.trim() || null; dlg.close(); } },
        input, h('button', { class: 'btn btn-primary', type: 'submit' }, action)),
      h('div', { class: 'dialog-actions' }, h('button', { class: 'btn btn-ghost', type: 'button', onclick: () => dlg.close() }, 'Cancel')));
    dlg.addEventListener('close', () => { dlg.remove(); resolve(result); });
    document.body.append(dlg);
    dlg.showModal();
    input.select();
  });
}
