// The Table: the library as a dense table, of tracks or of albums, artists, genres, years, …
// (rows grouped by a field, with how many tracks each has), from a `field:value` query.
// See "The Table" in docs/api.md.
//
// Its state is the address (#/library/table?q=…&group=…&cols=…&sort=…), so going into a
// row and coming back is the browser's back button, and a view can be bookmarked.  Only the
// visible rows are in the page; pages of 200 come from the server as they scroll into view.

import { h, icon, fmtTime, plural, storage, debounce, toast } from '../util.js';
import { queryTable, getQueryFields, getQueryViews, putQueryViews } from '../api.js';
import { openMenu, askName } from './menu.js';
import { enc, errorBox } from './common.js';

const ROW_H = 30;
const PAGE = 200;
/** Rows drawn beyond the visible ones, above and below. */
const OVERSCAN = 12;

/** Columns a grouping starts with. */
const DEFAULT_COLUMNS = {
  track: ['track', 'title', 'artist', 'album', 'year', 'genre', 'duration', 'format'],
  album: ['album', 'album_artist', 'count', 'year', 'genre', 'duration', 'format'],
  artist: ['artist', 'count', 'year', 'genre', 'duration', 'plays'],
  album_artist: ['album_artist', 'count', 'year', 'genre', 'duration', 'plays'],
  genre: ['genre', 'count', 'duration', 'plays'],
  format: ['format', 'count', 'duration', 'size', 'bitrate'],
  folder: ['folder', 'count', 'duration', 'size'],
};

/** Ready-made views; saved ones follow. */
const BUILT_IN = [
  { name: 'All tracks', q: '', group: 'track' },
  { name: 'Albums', q: '', group: 'album' },
  { name: 'Artists', q: '', group: 'artist' },
  { name: 'Genres', q: '', group: 'genre', sort: [{ field: 'count', desc: true }] },
  { name: 'Years', q: '', group: 'year', sort: [{ field: 'year', desc: true }] },
  { name: 'Recently added', q: 'added:<90d', group: 'album', sort: [{ field: 'added', desc: true }],
    columns: ['album', 'album_artist', 'count', 'added', 'year', 'format'] },
  { name: 'Most played', q: 'plays:>0', group: 'track', sort: [{ field: 'plays', desc: true }],
    columns: ['title', 'artist', 'album', 'plays', 'last_played', 'rating'] },
  { name: 'Never played', q: 'plays:0', group: 'album', sort: [{ field: 'added', desc: true }],
    columns: ['album', 'album_artist', 'count', 'added', 'year'] },
  { name: 'Albums with mixed artists', q: 'varies:album_artist', group: 'album',
    columns: ['album', 'album_artist', 'count', 'folder'] },
  { name: 'Missing years', q: 'missing:year', group: 'album', columns: ['album', 'album_artist', 'count', 'year', 'folder'] },
  { name: 'Lossy albums', q: 'lossless:no', group: 'album', columns: ['album', 'album_artist', 'count', 'format', 'bitrate'] },
];

const HELP = [
  ['blue train', 'words, searched as everywhere'],
  ['artist:coltrane', 'contains (any of the track’s artists)'],
  ['album:="a love supreme"', 'exactly'],
  ['genre:jazz,soul', 'any of them'],
  ['year:1955..1965', 'a range; also >, >=, <, <='],
  ['duration:>10:00  size:>1gb  bitrate:>=320', 'in their units'],
  ['added:<30d  last_played:>1y  added:2024', 'ages (h d w mo y) or dates'],
  ['lossless:yes', 'yes or no'],
  ['-format:mp3  -live', 'not'],
  ['missing:mbid  has:genre', 'without, with a value'],
  ['varies:album_artist', 'groups whose tracks differ'],
];

let fieldsPromise = null;
const fields = () => (fieldsPromise ||= getQueryFields().catch((e) => { fieldsPromise = null; throw e; }));

/** `title,-year` ↔ [{field: 'title'}, {field: 'year', desc: true}] */
const sortFrom = (s) => (s || '').split(',').filter(Boolean).map((f) => (f.startsWith('-') ? { field: f.slice(1), desc: true } : { field: f }));
const sortText = (sort) => sort.map((s) => (s.desc ? '-' : '') + s.field).join(',');

function hashFor(st) {
  const p = new URLSearchParams();
  if (st.q) p.set('q', st.q);
  if (st.group && st.group !== 'track') p.set('group', st.group);
  if (st.columns) p.set('cols', st.columns.join(','));
  if (st.sort?.length) p.set('sort', sortText(st.sort));
  if (st.view) p.set('view', st.view);
  if (st.title) p.set('in', st.title);
  const s = p.toString();
  return `#/library/table${s ? `?${s}` : ''}`;
}

// ---------------------------------------------------------------- cells

const STARS = (r) => '★★★★★'.slice(0, Math.round(r / 20)) + '☆☆☆☆☆'.slice(0, 5 - Math.round(r / 20));
const SIZE = (b) => (b >= 1e9 ? `${(b / 1e9).toFixed(1)} GB` : b >= 1e6 ? `${(b / 1e6).toFixed(0)} MB` : `${Math.round(b / 1e3)} kB`);
const DATE = (t) => new Date(t * 1000).toISOString().slice(0, 10);

/** One value of a field, as text. */
function show(f, v) {
  if (v == null) return '';
  switch (f.kind) {
    case 'duration': return fmtTime(v);
    case 'time': return DATE(v);
    case 'bool': return v ? '✓' : '—';
    case 'size': return SIZE(v);
    case 'bitrate': return `${Math.round(v / 1000)}`;
    case 'real': return f.name === 'bpm' ? String(Math.round(v)) : (+v).toFixed(1);
    case 'int':
      if (f.name === 'rating') return STARS(v);
      if (f.name === 'sample_rate') return `${(v / 1000).toLocaleString()} kHz`;
      return f.name === 'year' || f.name === 'decade' ? String(v) : (+v).toLocaleString();
    default:
      if (f.name === 'format') return FORMATS[v] || v;
      return String(v);
  }
}

const FORMATS = { flc: 'FLAC', mp3: 'MP3', mp4: 'AAC', alc: 'ALAC', ogg: 'Vorbis', ops: 'Opus', wav: 'WAV', aif: 'AIFF', wma: 'WMA', ape: 'APE', wvp: 'WavPack', dsf: 'DSD' };

/** A cell: a value, a range, or what varies. */
function cellNode(f, v) {
  if (v && typeof v === 'object' && !Array.isArray(v) && 'varies' in v) {
    return h('span', { class: 'tbl-varies', title: `${v.varies} different values` }, `‹${v.varies} values›`);
  }
  if (Array.isArray(v)) {
    const [a, b] = v.map((x) => show(f, x));
    return f.kind === 'int' && (f.name === 'year' || f.name === 'decade') ? `${a}–${b}` : `${a} – ${b}`;
  }
  return show(f, v);
}

const numeric = (f) => f && ['int', 'real', 'duration', 'size', 'bitrate'].includes(f.kind) && f.name !== 'year' && f.name !== 'decade';

function defaultWidth(f, grouped) {
  if (!f) return 76;
  // a grouped row may show a range (1974–2018)
  if (grouped && f.agg === 'range') return f.kind === 'real' ? 120 : 104;
  if (['title', 'album', 'path', 'folder'].includes(f.name)) return f.name === 'path' || f.name === 'folder' ? 340 : 240;
  if (['artist', 'album_artist', 'genre'].includes(f.name)) return 180;
  if (f.name === 'track' || f.name === 'disc' || f.name === 'channels' || f.name === 'bits') return 52;
  if (f.name === 'rating') return 90;
  if (f.kind === 'time') return 104;
  if (f.kind === 'text') return 140;
  return 84;
}

// ---------------------------------------------------------------- the view

export function render(v) {
  const q0 = v.query;
  const st = {
    q: q0.get('q') || '',
    group: q0.get('group') || 'track',
    columns: q0.get('cols') ? q0.get('cols').split(',').filter(Boolean) : null,
    sort: sortFrom(q0.get('sort')),
    view: q0.get('view') || '',
    // the name of the row this was opened from (an album, an artist, …)
    title: q0.get('in') || '',
  };
  v.crumbs([{ label: 'Table', href: '#/library/table' }, ...(st.title ? st.title.split(' › ').map((label) => ({ label })) : [])]);

  const input = h('input', {
    type: 'search', class: 'tbl-query', value: st.q, placeholder: 'Filter: words, or field:value (artist:miles year:1955..1965)',
    'aria-label': 'Query', spellcheck: false, autocomplete: 'off',
  });
  const errorLine = h('p', { class: 'tbl-error', role: 'alert', hidden: true });
  const groupSel = h('select', { 'aria-label': 'Show' });
  const summary = h('span', { class: 'tbl-summary muted', 'aria-live': 'polite' });
  const viewBtn = h('button', { class: 'btn', type: 'button', 'aria-haspopup': 'menu' }, icon('playlist'), h('span', {}, st.view || 'Views'));
  const colsBtn = h('button', { class: 'btn', type: 'button' }, icon('columns'), 'Columns');
  const helpBtn = h('button', { class: 'icon-btn', type: 'button', title: 'How to write a query', 'aria-label': 'How to write a query', 'aria-expanded': 'false' }, icon('help'));
  const playBtn = h('button', { class: 'btn', type: 'button', title: 'Play every track this finds' }, icon('play'), 'Play all');
  const addBtn = h('button', { class: 'icon-btn', type: 'button', title: 'Add every track this finds to the queue', 'aria-label': 'Add all to the queue' }, icon('add'));
  const help = h('div', { class: 'tbl-help', hidden: true },
    h('table', {}, h('tbody', {}, HELP.map(([code, what]) => h('tr', {}, h('td', {}, h('code', {}, code)), h('td', { class: 'muted' }, what))))),
    h('p', { class: 'muted small' }, 'Every filter must hold. Quote values with spaces; a word with a colon in it is quoted too: "re:birth".'));

  const head = h('div', { class: 'tbl-head', role: 'row' });
  const sizer = h('div', { class: 'tbl-sizer' });
  const bodyEl = h('div', { class: 'tbl-body', role: 'rowgroup' });
  const scroller = h('div', { class: 'tbl-scroll', tabindex: '0', role: 'grid', 'aria-label': 'Library table' }, head, sizer);
  sizer.append(bodyEl);
  const wrap = h('div', { class: 'tbl' },
    h('div', { class: 'tbl-bar' }, h('div', { class: 'tbl-query-row' }, input, helpBtn), help, errorLine,
      h('div', { class: 'tbl-tools' },
        h('label', { class: 'tbl-show' }, 'Show ', groupSel),
        viewBtn, colsBtn, playBtn, addBtn, summary)),
    scroller);
  v.body.append(wrap);

  let F = {};              // fields by name
  let cols = [];           // current columns (names)
  let widths = {};
  let total = 0;
  let pages = new Map();   // page number → rows
  let loading = new Map(); // page number → promise
  let generation = 0;      // bumps on every new query
  let focusRow = -1;

  const groupName = () => (st.group === 'track' ? 'track' : st.group);
  const colStore = () => `rms.table.cols.${groupName()}`;
  const widthStore = 'rms.table.widths';
  try { widths = JSON.parse(storage.get(widthStore, '{}')) || {}; } catch { widths = {}; }

  // ---- layout: the table fills the rest of the screen and scrolls inside
  const fit = () => {
    const main = v.scrollRoot;
    const top = scroller.getBoundingClientRect().top - main.getBoundingClientRect().top + main.scrollTop;
    scroller.style.height = `${Math.max(240, main.clientHeight - top - 16)}px`;
    draw();
  };
  const ro = new ResizeObserver(() => fit());
  ro.observe(v.scrollRoot);
  v.cleanup.push(() => ro.disconnect());

  const width = (c) => widths[c] || defaultWidth(F[c], st.group !== 'track');
  const template = () => cols.map((c) => `${Math.round(width(c))}px`).join(' ');
  const setTemplate = () => {
    const t = template();
    scroller.style.setProperty('--tbl-cols', t);
    const w = cols.reduce((n, c) => n + width(c), 0) + 44;
    scroller.style.setProperty('--tbl-width', `${w}px`);
  };

  function label(c) {
    if (c === 'count') return 'Tracks';
    return F[c]?.label || c;
  }

  // ---- header: sort on click (shift: add a key), drag the edge to resize
  function drawHead() {
    head.replaceChildren(...cols.map((c) => {
      const i = st.sort.findIndex((s) => s.field === c);
      const s = st.sort[i];
      const cell = h('div', {
        class: `tbl-th${numeric(F[c]) || c === 'count' ? ' num' : ''}`, role: 'columnheader', tabindex: '0',
        'aria-sort': s ? (s.desc ? 'descending' : 'ascending') : 'none',
        title: 'Sort (Shift: add to the sort)',
        onclick: (e) => { if (!e.target.closest('.tbl-grip')) sortBy(c, e.shiftKey); },
        onkeydown: (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); sortBy(c, e.shiftKey); } },
      }, h('span', { class: 'tbl-th-label' }, label(c)),
      s && h('span', { class: 'tbl-sort' }, s.desc ? '▾' : '▴', st.sort.length > 1 ? h('sub', {}, i + 1) : null));
      const grip = h('span', { class: 'tbl-grip', 'aria-hidden': 'true' });
      grip.addEventListener('pointerdown', (e) => {
        e.preventDefault();
        grip.setPointerCapture(e.pointerId);
        const x0 = e.clientX, w0 = width(c);
        const move = (ev) => { widths[c] = Math.max(40, w0 + ev.clientX - x0); setTemplate(); };
        const up = () => {
          grip.removeEventListener('pointermove', move);
          storage.set(widthStore, JSON.stringify(widths));
        };
        grip.addEventListener('pointermove', move);
        grip.addEventListener('pointerup', up, { once: true });
      });
      grip.addEventListener('dblclick', (e) => { e.stopPropagation(); delete widths[c]; setTemplate(); storage.set(widthStore, JSON.stringify(widths)); });
      cell.append(grip);
      return cell;
    }), h('div', { class: 'tbl-th tbl-th-end' }));
  }

  function sortBy(c, add) {
    const i = st.sort.findIndex((s) => s.field === c);
    let sort = add ? [...st.sort] : st.sort.filter((s) => s.field === c);
    if (i < 0) sort = add ? [...sort, { field: c }] : [{ field: c, desc: numeric(F[c]) || c === 'count' || F[c]?.kind === 'time' }];
    else {
      const cur = sort.find((s) => s.field === c);
      // ascending → descending → off
      if (cur.desc === (numeric(F[c]) || c === 'count' || F[c]?.kind === 'time')) sort = sort.map((s) => (s.field === c ? { ...s, desc: !s.desc } : s));
      else sort = sort.filter((s) => s.field !== c);
    }
    st.sort = sort;
    update(false);
  }

  // ---- rows
  async function page(n) {
    if (pages.has(n)) return pages.get(n);
    if (loading.has(n)) return loading.get(n);
    const gen = generation;
    const p = queryTable({ q: st.q, group: st.group, columns: cols, sort: st.sort, offset: n * PAGE, limit: PAGE })
      .then((r) => {
        if (gen !== generation) return null;
        pages.set(n, r.rows);
        loading.delete(n);
        return r;
      })
      .catch((e) => { loading.delete(n); throw e; });
    loading.set(n, p);
    return p;
  }

  function rowAt(i) {
    return pages.get(Math.floor(i / PAGE))?.[i % PAGE];
  }

  const rowNodes = new Map(); // index → element
  function draw() {
    if (!total) { bodyEl.replaceChildren(); rowNodes.clear(); return; }
    const top = scroller.scrollTop - head.offsetHeight;
    const first = Math.max(0, Math.floor(top / ROW_H) - OVERSCAN);
    const last = Math.min(total - 1, Math.ceil((top + scroller.clientHeight) / ROW_H) + OVERSCAN);
    for (const [i, el] of rowNodes) if (i < first || i > last) { el.remove(); rowNodes.delete(i); }
    const missing = new Set();
    for (let i = first; i <= last; i++) {
      const r = rowAt(i);
      let el = rowNodes.get(i);
      if (el && (el.dataset.ready === '1' || !r)) continue;
      if (!r) missing.add(Math.floor(i / PAGE));
      const fresh = rowNode(i, r);
      if (el) el.replaceWith(fresh); else bodyEl.append(fresh);
      rowNodes.set(i, fresh);
    }
    for (const n of missing) page(n).then((r) => r && draw()).catch(showError);
  }

  function rowNode(i, r) {
    const el = h('div', {
      class: `tbl-row${i % 2 ? ' odd' : ''}${i === focusRow ? ' focus' : ''}`, role: 'row', 'aria-rowindex': i + 2,
      style: { transform: `translateY(${i * ROW_H}px)` },
    });
    if (!r) {
      el.append(h('div', { class: 'tbl-td tbl-loading' }, '…'));
      return el;
    }
    el.dataset.ready = '1';
    r.cells.forEach((v, k) => {
      const f = F[cols[k]];
      const content = cols[k] === 'count' ? (+v).toLocaleString() : cellNode(f || { kind: 'text', name: cols[k] }, v);
      const text = typeof content === 'string' ? content : '';
      el.append(h('div', { class: `tbl-td${numeric(f) || cols[k] === 'count' ? ' num' : ''}${cols[k] === 'rating' ? ' stars' : ''}`, role: 'gridcell', title: text.length > 24 ? text : null }, content));
    });
    const more = h('button', { class: 'icon-btn tbl-more', type: 'button', 'aria-label': 'More', title: 'More', tabindex: '-1',
      onclick: (e) => { e.stopPropagation(); rowMenu(r, more); } }, icon('more'));
    el.append(h('div', { class: 'tbl-td tbl-td-end' }, more));
    el.addEventListener('click', () => setFocus(i));
    el.addEventListener('dblclick', () => activate(r));
    el.addEventListener('contextmenu', (e) => { e.preventDefault(); setFocus(i); rowMenu(r, { x: e.clientX, y: e.clientY }); });
    return el;
  }

  function setFocus(i) {
    rowNodes.get(focusRow)?.classList.remove('focus');
    focusRow = i;
    rowNodes.get(i)?.classList.add('focus');
  }

  const rowName = (r) => (r.id != null ? String(r.cells[cols.indexOf('title')] ?? 'track') : r.label == null ? `(no ${label(st.group).toLowerCase()})` : cellNode(F[st.group], r.label));
  const rowQuery = (r) => [st.q, r.drill].filter(Boolean).join(' ');

  /** Double-click or Enter: play a track; open a group. */
  function activate(r) {
    if (r.id != null) v.ctx.playItem('load', 'track_id', r.id, rowName(r));
    else drill(r);
  }

  function drill(r) {
    const next = r.next || 'track';
    // the trail of rows opened: Artist › Album
    const title = [st.title, String(rowName(r))].filter(Boolean).join(' › ');
    v.ctx.navigate(hashFor({ q: rowQuery(r), group: next, columns: null, sort: [], title }));
  }

  function rowMenu(r, at) {
    const name = String(rowName(r));
    if (r.id != null) {
      const go = (action) => () => v.ctx.playItem(action, 'track_id', r.id, name);
      openMenu(at, name, [
        { label: 'Play now', icon: 'play', run: go('load') },
        { label: 'Play next', icon: 'playnext', run: go('insert') },
        { label: 'Add to queue', icon: 'add', run: go('add') },
        { label: 'Start a mix', icon: 'mix', run: () => v.ctx.createMix('track_id', r.id, name) },
        'sep',
        { label: 'Analysis details', icon: 'note', run: () => { location.hash = `#/library/track/${enc(r.id)}?name=${enc(name)}`; } },
      ]);
      return;
    }
    const item = { q: rowQuery(r), sort: [] };
    const go = (action) => () => v.ctx.playItem(action, 'query', item, name);
    openMenu(at, name, [
      { label: `Show its ${r.next === 'album' ? 'albums' : 'tracks'}`, icon: 'chevron', run: () => drill(r) },
      'sep',
      { label: 'Play now', icon: 'play', run: go('load') },
      { label: 'Play next', icon: 'playnext', run: go('insert') },
      { label: 'Add to queue', icon: 'add', run: go('add') },
    ]);
  }

  // ---- keyboard: up/down/page/home/end move, Enter opens or plays
  scroller.addEventListener('keydown', (e) => {
    if (!total || e.target !== scroller) return;
    const visible = Math.max(1, Math.floor((scroller.clientHeight - head.offsetHeight) / ROW_H));
    const moves = { ArrowDown: 1, ArrowUp: -1, PageDown: visible, PageUp: -visible, Home: -Infinity, End: Infinity };
    if (e.key in moves) {
      e.preventDefault();
      const i = Math.max(0, Math.min(total - 1, (focusRow < 0 ? -1 : focusRow) + moves[e.key]));
      setFocus(i);
      const y = i * ROW_H;
      const viewTop = scroller.scrollTop, viewBottom = viewTop + scroller.clientHeight - head.offsetHeight;
      if (y < viewTop) scroller.scrollTop = y;
      else if (y + ROW_H > viewBottom) scroller.scrollTop = y + ROW_H - (scroller.clientHeight - head.offsetHeight);
      draw();
    } else if (e.key === 'Enter' && focusRow >= 0) {
      const r = rowAt(focusRow);
      if (r) activate(r);
    } else if ((e.key === 'ContextMenu' || (e.shiftKey && e.key === 'F10')) && focusRow >= 0) {
      e.preventDefault();
      const r = rowAt(focusRow);
      const el = rowNodes.get(focusRow);
      if (r && el) rowMenu(r, el.querySelector('.tbl-more'));
    }
  });
  scroller.addEventListener('scroll', () => requestAnimationFrame(draw), { passive: true });

  function showError(e) {
    errorLine.textContent = e.message;
    errorLine.hidden = false;
    input.setAttribute('aria-invalid', 'true');
  }

  // ---- a new query, grouping, columns or sort
  /** `push`: a new history entry (grouping, view); otherwise the address is replaced. */
  async function update(push) {
    const next = hashFor({ ...st, columns: st.columns });
    if (push) v.ctx.navigate(next);
    else history.replaceState(null, '', next);
    if (push) return; // the new address renders afresh
    generation++;
    pages = new Map();
    loading = new Map();
    focusRow = -1;
    cols = st.columns || JSON.parse(storage.get(colStore(), 'null') || 'null') || DEFAULT_COLUMNS[groupName()] || [st.group, 'count', 'duration'];
    cols = cols.filter((c) => c === 'count' ? st.group !== 'track' : F[c]);
    if (!cols.length) cols = DEFAULT_COLUMNS.track;
    setTemplate();
    drawHead();
    try {
      const r = await page(0);
      if (!r) return;
      errorLine.hidden = true;
      input.removeAttribute('aria-invalid');
      total = r.total;
      sizer.style.height = `${total * ROW_H}px`;
      rowNodes.clear();
      bodyEl.replaceChildren();
      scroller.scrollTop = 0;
      const what = st.group === 'track' ? 'track' : (F[st.group]?.label || st.group).toLowerCase();
      const tracks = r.tracks >= 0 && st.group !== 'track' ? ` · ${plural(r.tracks, 'track')}` : '';
      summary.textContent = `${plural(total, what)}${tracks} · ${fmtLong(r.duration)}`;
      if (!total) bodyEl.append(h('div', { class: 'tbl-empty muted' }, st.q ? 'Nothing matches.' : 'Your library is empty.'));
      draw();
    } catch (e) {
      total = 0;
      sizer.style.height = '0px';
      bodyEl.replaceChildren();
      summary.textContent = '';
      showError(e);
    }
  }

  const fmtLong = (secs) => {
    const d = Math.floor(secs / 86400), hr = Math.round((secs % 86400) / 3600);
    return d ? `${d} d ${hr} h` : secs >= 3600 ? `${(secs / 3600).toFixed(1)} h` : fmtTime(secs);
  };

  // ---- the controls
  const typed = debounce(() => { st.q = input.value.trim(); st.view = ''; update(false); }, 300);
  input.addEventListener('input', typed);
  input.addEventListener('keydown', (e) => { if (e.key === 'Enter') { typed.cancel(); st.q = input.value.trim(); update(false); } });
  helpBtn.addEventListener('click', () => {
    help.hidden = !help.hidden;
    helpBtn.setAttribute('aria-expanded', String(!help.hidden));
    fit();
  });
  groupSel.addEventListener('change', () => {
    st.group = groupSel.value;
    st.columns = null;
    st.sort = [];
    st.view = '';
    st.title = '';
    update(true);
  });
  const queryItem = () => ({ q: st.q, sort: st.sort });
  const what = () => st.title.split(' › ').pop() || st.view || st.q || 'the library';
  playBtn.addEventListener('click', () => v.ctx.playItem('load', 'query', queryItem(), what()));
  addBtn.addEventListener('click', () => v.ctx.playItem('add', 'query', queryItem(), what()));
  colsBtn.addEventListener('click', () => chooseColumns());
  viewBtn.addEventListener('click', () => viewMenu());

  async function chooseColumns() {
    const all = Object.values(F);
    const chosen = [...cols];
    const list = h('ul', { class: 'tbl-colpick' });
    const renderList = () => {
      const order = [...chosen, ...(st.group !== 'track' && !chosen.includes('count') ? ['count'] : []),
        ...all.map((f) => f.name).filter((n) => !chosen.includes(n))];
      list.replaceChildren(...order.map((name) => {
        const on = chosen.includes(name);
        const i = chosen.indexOf(name);
        const id = `col-${name}`;
        return h('li', {},
          h('input', { type: 'checkbox', id, checked: on, onchange: (e) => {
            if (e.target.checked) chosen.push(name); else chosen.splice(chosen.indexOf(name), 1);
            renderList();
          } }),
          h('label', { for: id }, label(name)),
          on && h('span', { class: 'tbl-colmove' },
            h('button', { class: 'icon-btn', type: 'button', 'aria-label': `Move ${label(name)} left`, disabled: i === 0,
              onclick: () => { chosen.splice(i, 1); chosen.splice(i - 1, 0, name); renderList(); } }, '↑'),
            h('button', { class: 'icon-btn', type: 'button', 'aria-label': `Move ${label(name)} right`, disabled: i === chosen.length - 1,
              onclick: () => { chosen.splice(i, 1); chosen.splice(i + 1, 0, name); renderList(); } }, '↓')));
      }));
    };
    renderList();
    const ok = await new Promise((resolve) => {
      let result = false;
      const dlg = h('dialog', { class: 'dialog tbl-dialog', 'aria-label': 'Columns' },
        h('h2', {}, 'Columns'), list,
        h('div', { class: 'dialog-actions' },
          h('button', { class: 'btn btn-ghost', type: 'button', onclick: () => { chosen.splice(0, chosen.length, ...(DEFAULT_COLUMNS[groupName()] || [st.group, 'count'])); renderList(); } }, 'Defaults'),
          h('button', { class: 'btn btn-ghost', type: 'button', onclick: () => dlg.close() }, 'Cancel'),
          h('button', { class: 'btn btn-primary', type: 'button', onclick: () => { result = true; dlg.close(); } }, 'Show')));
      dlg.addEventListener('close', () => { dlg.remove(); resolve(result); });
      document.body.append(dlg);
      dlg.showModal();
    });
    if (!ok || !chosen.length) return;
    st.columns = chosen;
    storage.set(colStore(), JSON.stringify(chosen));
    update(false);
  }

  async function viewMenu() {
    let saved = [];
    try { saved = await getQueryViews(); } catch { /* offline: built-ins only */ }
    const apply = (vw) => () => v.ctx.navigate(hashFor({
      q: vw.q || '', group: vw.group || 'track', columns: vw.columns?.length ? vw.columns : null, sort: vw.sort || [], view: vw.name,
    }));
    const save = async () => {
      const name = await askName('Save this view as', st.view && !BUILT_IN.some((b) => b.name === st.view) ? st.view : '');
      if (!name) return;
      const vw = { name, q: st.q, group: st.group, columns: cols, sort: st.sort, widths: Object.fromEntries(cols.filter((c) => widths[c]).map((c) => [c, widths[c]])) };
      const rest = saved.filter((s) => s.name.toLowerCase() !== name.toLowerCase());
      try {
        await putQueryViews([...rest, vw]);
        st.view = name;
        viewBtn.querySelector('span').textContent = name;
        history.replaceState(null, '', hashFor(st));
        toast(`Saved “${name}”`);
      } catch (e) { toast(`Could not save the view: ${e.message}`, 'error'); }
    };
    const remove = (name) => async () => {
      try {
        await putQueryViews(saved.filter((s) => s.name !== name));
        toast(`Deleted “${name}”`);
      } catch (e) { toast(`Could not delete the view: ${e.message}`, 'error'); }
    };
    const mine = saved.find((s) => s.name === st.view);
    openMenu(viewBtn, 'Views', [
      { heading: 'Views' },
      ...BUILT_IN.map((b) => ({ label: b.name, checked: st.view === b.name, run: apply(b) })),
      saved.length && { heading: 'Saved' },
      ...saved.map((s) => ({ label: s.name, checked: st.view === s.name, run: () => {
        if (s.widths) { Object.assign(widths, s.widths); storage.set(widthStore, JSON.stringify(widths)); }
        apply(s)();
      } })),
      'sep',
      { label: 'Save this view…', icon: 'add', run: save },
      mine && { label: `Delete “${mine.name}”`, icon: 'close', danger: true, run: remove(mine.name) },
    ], { align: 'start' });
  }

  // ---- start
  fields().then((list) => {
    if (!v.alive) return;
    F = Object.fromEntries(list.map((f) => [f.name, f]));
    const groups = list.filter((f) => f.next);
    const plural_ = (f) => ({ album: 'Albums', artist: 'Artists', album_artist: 'Album artists', genre: 'Genres', year: 'Years',
      decade: 'Decades', format: 'Formats', folder: 'Folders', key: 'Keys', rating: 'Ratings', release_type: 'Release types' }[f.name] || f.label);
    groupSel.replaceChildren(h('option', { value: 'track' }, 'Tracks'), ...groups.map((f) => h('option', { value: f.name }, plural_(f))));
    groupSel.value = F[st.group]?.next ? st.group : 'track';
    st.group = groupSel.value;
    // a view's widths come with it
    requestAnimationFrame(fit);
    update(false);
  }).catch((e) => {
    if (v.alive) v.body.replaceChildren(errorBox(e, () => v.ctx.reload()));
  });
}
