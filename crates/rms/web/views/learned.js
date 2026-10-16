// The Spotify tracks learned for mixes (#/library/spotify-learned): what their sound was
// analysed into, in a table to sort (click a heading) and filter.

import { getSpotifyLearned, sizedArtwork } from '../api.js';
import { withMenu, trackEntries } from './menu.js';
import { h, fmtTime, plural, storage } from '../util.js';
import { emptyBox, lazyArt } from './common.js';

const SORT_KEY = 'rms.learned.sort';

const date = (secs) => new Date(secs * 1000).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });

/** [key, heading, cell text, sort value, numeric] */
const COLUMNS = [
  ['title', 'Title', (t) => t.title, (t) => t.title.toLowerCase()],
  ['artist', 'Artist', (t) => t.artist, (t) => t.artist.toLowerCase()],
  ['album', 'Album', (t) => t.album || '', (t) => (t.album || '').toLowerCase()],
  ['year', 'Year', (t) => t.year ?? '', (t) => t.year, true],
  ['duration', 'Length', (t) => (t.duration != null ? fmtTime(t.duration) : ''), (t) => t.duration, true],
  ['bpm', 'BPM', (t) => (t.bpm != null ? Math.round(t.bpm) : ''), (t) => t.bpm, true],
  ['key', 'Key', (t) => t.key || '', (t) => camelotOrder(t.key)],
  ['loudness', 'LUFS', (t) => (t.loudness != null ? t.loudness.toFixed(1) : ''), (t) => t.loudness, true],
  ['style', 'Style', (t) => t.style || '', (t) => (t.style || '').toLowerCase()],
  ['genres', 'Genres', (t) => t.genres.join(', '), (t) => t.genres.join(', ').toLowerCase()],
  ['in_library', 'In library', (t) => (t.in_library ? '✓' : ''), (t) => (t.in_library ? 1 : 0)],
  ['learned_at', 'Learned', (t) => date(t.learned_at), (t) => t.learned_at, true],
];

/** Camelot keys in wheel order: 1A 1B 2A … */
function camelotOrder(k) {
  const m = /^(\d+)([AB])$/.exec(k || '');
  return m ? +m[1] * 2 + (m[2] === 'B' ? 1 : 0) : null;
}

export async function learnedPage(v) {
  const r = await getSpotifyLearned();
  if (!v.alive) return;
  const items = r.items;
  if (!items.length) {
    v.body.append(emptyBox(h('div', {},
      h('p', {}, 'No Spotify tracks learned yet.'),
      h('p', { class: 'muted' }, 'With ', h('a', { href: '#/settings' }, 'Settings → Spotify → Use what I play in mixes'),
        ' on, each Spotify track that plays to its end is analysed for mixes.'))));
    return;
  }

  let sort = { key: 'learned_at', desc: true };
  try { sort = { ...sort, ...JSON.parse(storage.get(SORT_KEY) || '{}') }; } catch { /* keep the default */ }
  let filter = '';

  const input = h('input', {
    type: 'search', class: 'tbl-query', placeholder: 'Filter by title, artist, album, style or genre',
    'aria-label': 'Filter the learned tracks', oninput: () => { filter = input.value.trim().toLowerCase(); fill(); },
  });
  const summary = h('span', { class: 'tbl-summary muted', 'aria-live': 'polite' });
  const headRow = h('tr');
  const tbody = h('tbody');
  const table = h('table', { class: 'lt-table' }, h('thead', {}, headRow), tbody);
  v.body.append(h('div', { class: 'tbl lt' },
    h('div', { class: 'tbl-bar' }, h('div', { class: 'tbl-query-row' }, input), summary),
    h('div', { class: 'lt-scroll', tabindex: '0' }, table)));

  function heads() {
    headRow.replaceChildren(h('th', { class: 'lt-art', 'aria-label': 'Cover' }), ...COLUMNS.map(([key, label, , , num]) => {
      const on = sort.key === key;
      return h('th', {
        class: num ? 'num' : null, scope: 'col', tabindex: '0',
        'aria-sort': on ? (sort.desc ? 'descending' : 'ascending') : 'none',
        onclick: () => sortBy(key, num),
        onkeydown: (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); sortBy(key, num); } },
      }, label, on && h('span', { class: 'tbl-sort' }, sort.desc ? ' ▾' : ' ▴'));
    }), h('th', { class: 'lt-end', 'aria-label': 'Actions' }));
  }

  function sortBy(key, num) {
    // numbers and dates start from the highest, text from A
    sort = sort.key === key ? { key, desc: !sort.desc } : { key, desc: !!num };
    storage.set(SORT_KEY, JSON.stringify(sort));
    heads();
    fill();
  }

  function fill() {
    const col = COLUMNS.find(([k]) => k === sort.key) || COLUMNS[COLUMNS.length - 1];
    const val = col[3];
    const shown = items.filter((t) => !filter
      || [t.title, t.artist, t.album, t.style, ...t.genres].some((s) => s && s.toLowerCase().includes(filter)));
    shown.sort((a, b) => {
      const x = val(a), y = val(b);
      // unknowns last, whichever way
      if (x == null || x === '') return y == null || y === '' ? 0 : 1;
      if (y == null || y === '') return -1;
      const c = x < y ? -1 : x > y ? 1 : 0;
      return sort.desc ? -c : c;
    });
    summary.textContent = shown.length === items.length
      ? plural(items.length, 'track') : `${shown.length.toLocaleString()} of ${plural(items.length, 'track')}`;
    tbody.replaceChildren(...shown.map((t) => {
      const art = lazyArt(t.artwork ? sizedArtwork(t.artwork, 80) : null, { cls: 'lt-cover', fallback: 'note' });
      const end = h('td', { class: 'lt-end' });
      const tr = h('tr', {
        tabindex: '0', title: `Play ${t.title}`,
        onclick: () => v.ctx.playItem('load', 'url', t.url, t.title),
        onkeydown: (e) => { if (e.key === 'Enter') v.ctx.playItem('load', 'url', t.url, t.title); },
      }, h('td', { class: 'lt-art' }, art), ...COLUMNS.map(([key, , text, , num]) =>
        h('td', { class: [num && 'num', key === 'in_library' && 'lt-yes'].filter(Boolean).join(' ') || null }, String(text(t)))), end);
      return withMenu(tr, () => [t.title, trackEntries(v.ctx, { url: t.url, title: t.title }, [
        { label: 'Play next', icon: 'add', run: () => v.ctx.playItem('insert', 'url', t.url, t.title) },
      ])], end);
    }));
    if (!shown.length) tbody.append(h('tr', {}, h('td', { class: 'tbl-empty muted', colspan: COLUMNS.length + 2 }, 'Nothing matches.')));
  }

  heads();
  fill();
}
