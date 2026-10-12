// An album's tags: what the files say next to what its MusicBrainz release says, written
// into the files when applied (Settings → Library → "Change tags in music files", with a
// login).  Route: #/library/tags/<album id>?name=

import { getAlbumRelease, setAlbumRelease, getAlbumTags, writeTags, undoTagEdit } from '../api.js';
import { h, icon, toast, plural } from '../util.js';
import { enc, emptyBox, loadingBox } from './common.js';

/** @typedef {import('../api-types').AlbumTags} AlbumTags */
/** @typedef {import('../api-types').ReleaseInfo} ReleaseInfo */

const LABELS = {
  title: 'Title',
  artist: 'Artist',
  album: 'Album',
  album_artist: 'Album artist',
  track_number: 'Track',
  track_total: 'Tracks',
  disc_number: 'Disc',
  disc_total: 'Discs',
  date: 'Date',
  original_date: 'Original date',
  label: 'Label',
  catalog_number: 'Catalog number',
  barcode: 'Barcode',
  musicbrainz_recording_id: 'MusicBrainz recording',
  musicbrainz_track_id: 'MusicBrainz track',
  musicbrainz_release_id: 'MusicBrainz release',
  musicbrainz_release_group_id: 'MusicBrainz release group',
  musicbrainz_artist_id: 'MusicBrainz artist',
  musicbrainz_album_artist_id: 'MusicBrainz album artist',
};

/** Tags with one value for the whole album: shown once, written to every file. */
const ALBUM_FIELDS = ['album', 'album_artist', 'date', 'original_date', 'track_total', 'disc_total',
  'label', 'catalog_number', 'barcode', 'musicbrainz_release_id', 'musicbrainz_release_group_id',
  'musicbrainz_album_artist_id'];
/** Identifiers: folded away, they matter to software rather than to reading. */
const ID_FIELDS = new Set(['musicbrainz_recording_id', 'musicbrainz_track_id', 'musicbrainz_release_id',
  'musicbrainz_release_group_id', 'musicbrainz_artist_id', 'musicbrainz_album_artist_id', 'barcode',
  'catalog_number']);

/** "Nevermind — Nirvana · 1991-09-24 · US · CD · 12 tracks" */
export function releaseLabel(r) {
  return [`${r.title} — ${r.artist}`, r.date, r.country, r.format, plural(r.track_count, 'track')]
    .filter(Boolean).join(' · ');
}

const MBID = /([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})/i;

export async function tagsPage(v, id) {
  const name = v.query.get('name') || 'Album';
  v.crumbs([
    { label: 'Albums', href: '#/library/albums' },
    { label: name, href: `#/library/album/${enc(id)}?name=${enc(name)}` },
    { label: 'Tags' },
  ]);
  let releaseId = v.query.get('release') || null;
  let showAll = false;
  let match = null;
  let data = null;
  /** track_id → field → {on, input} */
  let rows = new Map();
  let lastResult = null;

  const top = h('div', { class: 'tag-top' });
  const tracksEl = h('div', { class: 'tag-tracks' });
  const footer = h('div', { class: 'tag-footer' });
  v.body.replaceChildren(h('header', { class: 'page-head' },
    h('div', { class: 'page-head-text' }, h('h1', {}, `Tags of ${name}`),
      h('div', { class: 'page-sub muted' }, 'What the files say, next to what the album’s MusicBrainz release says. Tick what to change, edit any value, then apply.'))),
  top, tracksEl, footer);

  async function load() {
    tracksEl.replaceChildren(loadingBox());
    try {
      [match, data] = await Promise.all([getAlbumRelease(id), getAlbumTags(id, releaseId)]);
    } catch (e) {
      if (v.alive) tracksEl.replaceChildren(emptyBox(`Could not load the tags: ${e.message}`));
      return;
    }
    if (!v.alive) return;
    if (!data.release) showAll = true;
    render();
  }

  function render() {
    renderTop();
    rows = new Map(data.tracks.map((t) => [t.track_id, new Map()]));
    tracksEl.replaceChildren(renderAlbum(), ...data.tracks.map(renderTrack));
    renderFooter();
  }

  function renderTop() {
    const items = [];
    if (data.blocked) {
      items.push(h('div', { class: 'notice', role: 'note' }, data.blocked, ' ',
        h('a', { href: '#/settings' }, 'Settings')));
    }
    // the release the proposals come from, and the others considered
    const cands = match?.candidates || [];
    const current = data.release?.mbid;
    if (cands.length || data.release) {
      const options = cands.filter((c) => c.release).map((c) => h('option', { value: c.mbid, selected: c.mbid === current },
        `${releaseLabel(c.release)} (${Math.round(c.score * 100)}%)`));
      if (current && !cands.some((c) => c.mbid === current)) {
        options.unshift(h('option', { value: current, selected: true }, releaseLabel(data.release)));
      }
      const select = h('select', { 'aria-label': 'Release', onchange: (e) => { releaseId = e.target.value; load(); } }, options);
      items.push(h('div', { class: 'field' },
        h('label', {}, 'MusicBrainz release'),
        h('div', { class: 'inline-row' }, select,
          data.release && h('a', { class: 'btn', href: data.release.url, target: '_blank', rel: 'noopener' }, icon('link'), 'MusicBrainz')),
        match?.status === 'uncertain' && h('p', { class: 'muted small' }, 'Not a sure match: check it is your edition.')));
    } else {
      items.push(h('p', { class: 'muted' }, match?.status === 'pending'
        ? 'Not matched to a MusicBrainz release yet (turn on MusicBrainz identification in Settings, or give the release below).'
        : 'No MusicBrainz release found for this album: give it below, or edit the tags by hand.'));
    }
    // any release, by id or link
    const other = h('input', { type: 'text', placeholder: 'Release id or musicbrainz.org link', spellcheck: 'false', autocomplete: 'off' });
    items.push(h('form', { class: 'inline-row', onsubmit: async (e) => {
      e.preventDefault();
      const m = MBID.exec(other.value);
      if (!m) { toast('That is not a MusicBrainz release id', 'error'); return; }
      try {
        await setAlbumRelease(id, m[1].toLowerCase());
        releaseId = null;
        toast('Release chosen for the album');
        load();
      } catch (err) { toast(`Could not use it: ${err.message}`, 'error'); }
    } }, other, h('button', { class: 'btn', type: 'submit' }, 'Use this release')));
    const all = h('input', { type: 'checkbox', id: 'tag-show-all', checked: showAll, onchange: (e) => { showAll = e.target.checked; render(); } });
    items.push(h('div', { class: 'field check' }, all, h('label', { for: 'tag-show-all' }, 'Show every tag (to edit by hand)')));
    top.replaceChildren(...items);
  }

  /** A row: tick, name, value now, new value.  `targets`: [track_id, current] it writes to. */
  function tagRow(field, current, proposed, changed, disabled, targets) {
    const label = LABELS[field] || field;
    const box = h('input', { type: 'checkbox', checked: changed && !disabled, disabled, 'aria-label': `Change ${label}` });
    const input = h('input', { type: 'text', value: proposed ?? (current ?? ''), disabled, spellcheck: 'false',
      'aria-label': `New ${label}`, oninput: () => { box.checked = true; renderFooter(); } });
    box.addEventListener('change', renderFooter);
    for (const [trackId, cur] of targets) rows.get(trackId)?.set(field, { box, input, current: cur ?? '' });
    return h('tr', { class: changed ? 'changed' : '' },
      h('td', {}, box),
      h('th', { scope: 'row' }, label),
      h('td', { class: 'tag-current' }, current ?? h('span', { class: 'muted' }, '—')),
      h('td', { class: 'tag-new' }, input));
  }

  /** Rows, the identifiers folded under a summary. */
  function tagTable(trs, ids) {
    const head = h('thead', {}, h('tr', {}, h('th', {}, h('span', { class: 'sr-only' }, 'Change')), h('th', {}, 'Tag'), h('th', {}, 'Now'), h('th', {}, 'New')));
    return [
      trs.length ? h('table', { class: 'tag-table' }, head, h('tbody', {}, trs)) : null,
      ids.length ? h('details', { class: 'tag-ids' },
        h('summary', {}, `MusicBrainz ids and catalog numbers (${ids.length})`),
        h('table', { class: 'tag-table' }, h('tbody', {}, ids))) : null,
    ];
  }

  /** The tags the album's files share, once. */
  function renderAlbum() {
    const writable = data.tracks.filter((t) => !t.error);
    const disabled = !!data.blocked || !writable.length;
    const trs = [], ids = [];
    for (const field of ALBUM_FIELDS) {
      const vals = data.tracks.map((t) => t.tags.find((x) => x.field === field));
      const currents = [...new Set(vals.map((x) => x?.current ?? null))];
      const current = currents.length === 1 ? currents[0] : `${currents.length} different values`;
      const proposed = vals.find((x) => x?.proposed != null)?.proposed ?? null;
      const changed = vals.some((x) => x?.changed);
      if (!changed && !showAll) continue;
      const row = tagRow(field, current, proposed ?? (currents.length === 1 ? currents[0] : ''), changed, disabled,
        writable.map((t) => [t.track_id, t.tags.find((x) => x.field === field)?.current]));
      (ID_FIELDS.has(field) ? ids : trs).push(row);
    }
    return h('section', { class: 'tag-track' },
      h('h3', {}, 'Whole album', h('span', { class: 'muted small' }, ` ${plural(data.tracks.length, 'file')}`)),
      ...(trs.length || ids.length ? tagTable(trs, ids) : [h('p', { class: 'muted small' }, 'Nothing to change.')]));
  }

  function renderTrack(t) {
    const file = t.path.split('/').pop() || `Track ${t.track_id}`;
    const title = t.tags.find((x) => x.field === 'title');
    const disabled = !!(t.error || data.blocked);
    const trs = [], ids = [];
    for (const x of t.tags) {
      if (ALBUM_FIELDS.includes(x.field) || !(x.changed || showAll)) continue;
      const row = tagRow(x.field, x.current, x.proposed, x.changed, disabled, disabled ? [] : [[t.track_id, x.current]]);
      (ID_FIELDS.has(x.field) ? ids : trs).push(row);
    }
    return h('section', { class: 'tag-track' },
      h('h3', {}, title?.proposed || title?.current || file, h('span', { class: 'muted small' }, ` ${file}`)),
      t.error && h('p', { class: 'muted small', role: 'note' }, t.error),
      ...(trs.length || ids.length ? tagTable(trs, ids) : [h('p', { class: 'muted small' }, 'Nothing to change.')]));
  }

  function changes() {
    const out = [];
    for (const [track_id, fields] of rows) {
      const tags = {};
      for (const [field, f] of fields) {
        if (!f.box.checked) continue;
        const value = f.input.value.trim();
        if (value === f.current.trim()) continue;
        tags[field] = value === '' ? null : value;
      }
      if (Object.keys(tags).length) out.push({ track_id, tags });
    }
    return out;
  }

  function renderFooter() {
    const c = changes();
    const n = c.reduce((s, x) => s + Object.keys(x.tags).length, 0);
    const apply = h('button', { class: 'btn btn-primary', type: 'button', disabled: !n || !!data.blocked, onclick: () => applyChanges(c, apply) },
      icon('edit'), n ? `Change ${plural(n, 'tag')} in ${plural(c.length, 'file')}` : 'Nothing to change');
    const pick = (on) => () => {
      for (const fields of rows.values()) for (const f of fields.values()) if (!f.box.disabled) f.box.checked = on;
      renderFooter();
    };
    footer.replaceChildren(resultNotice() || '', h('div', { class: 'button-row' }, apply,
      h('button', { class: 'btn', type: 'button', onclick: pick(true), disabled: !!data.blocked }, 'Select all'),
      h('button', { class: 'btn', type: 'button', onclick: pick(false), disabled: !!data.blocked }, 'Select none')));
  }

  async function applyChanges(c, btn) {
    btn.disabled = true;
    try {
      // proposals from another release than the album's: it becomes the album's
      if (data.release && match?.release?.mbid !== data.release.mbid) {
        await setAlbumRelease(id, data.release.mbid);
      }
      const r = await writeTags(c);
      if (!v.alive) return;
      lastResult = r;
      if (r.failed?.length) toast(`${plural(r.failed.length, 'file')} not changed`, 'error');
      // the library picks the changes up as it rescans the folder
      setTimeout(() => v.alive && load(), 1500);
      renderFooter();
    } catch (e) {
      toast(`Could not change the tags: ${e.message}`, 'error');
      btn.disabled = false;
    }
  }

  /** What the last apply did, with its undo. */
  function resultNotice() {
    const r = lastResult;
    if (!r) return null;
    const undo = r.batch != null && h('button', { class: 'btn', type: 'button', onclick: async (e) => {
      e.target.disabled = true;
      try {
        const u = await undoTagEdit(r.batch);
        toast(`Put back the tags of ${plural(u.changed, 'file')}`);
        lastResult = null;
        setTimeout(() => v.alive && load(), 1500);
      } catch (err) { toast(`Could not undo: ${err.message}`, 'error'); e.target.disabled = false; }
    } }, 'Undo');
    return h('div', { class: 'notice', role: 'status' },
      h('p', {}, `Changed the tags of ${plural(r.changed, 'file')}.`),
      r.failed?.length ? h('ul', {}, r.failed.map((f) => h('li', {}, f.error))) : null,
      undo);
  }

  await load();
}
