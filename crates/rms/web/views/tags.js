// An album's tags: what the files say next to what its MusicBrainz release says, written
// into the files when applied (Settings → Library → "Change tags in music files", with a
// login).  Route: #/library/tags/<album id>?name=

import {
  getAlbumRelease, setAlbumRelease, getAlbumTags, writeTags, undoTagEdit, getAlbumFiles, moveAlbumFiles,
  patchSettings, getAlbums, getAlbum, getCoverArt, setCoverArt, sizedArtwork,
} from '../api.js';
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

/** What the last move did, carried to the album's page under its new id. */
let carried = null;

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
  let lastResult = carried;
  carried = null;
  let template = null;

  const top = h('div', { class: 'tag-top' });
  const tracksEl = h('div', { class: 'tag-tracks' });
  const footer = h('div', { class: 'tag-footer' });
  const coverEl = h('section', { class: 'tag-files' });
  const filesEl = h('section', { class: 'tag-files' });
  v.body.replaceChildren(h('header', { class: 'page-head' },
    h('div', { class: 'page-head-text' }, h('h1', {}, `Tags of ${name}`),
      h('div', { class: 'page-sub muted' }, 'What the files say, next to what the album’s MusicBrainz release says. Tick what to change, edit any value, then apply.'))),
  top, tracksEl, footer, coverEl, filesEl);

  async function load() {
    tracksEl.replaceChildren(loadingBox());
    try {
      [match, data] = await Promise.all([getAlbumRelease(id), getAlbumTags(id, releaseId)]);
    } catch (e) {
      if (v.alive) tracksEl.replaceChildren(emptyBox(`Could not load the tags: ${e.message}`));
      return;
    }
    if (!v.alive) return;
    render();
    loadFiles();
    loadCover();
  }

  // ---------------------------------------------------------------- cover art

  let coverResult = null;

  async function loadCover() {
    const title = h('h2', {}, 'Cover art');
    let art, album;
    try {
      [art, album] = await Promise.all([getCoverArt(id, releaseId), getAlbum(id)]);
    } catch (e) {
      if (v.alive) coverEl.replaceChildren(title, h('p', { class: 'muted' }, `Could not load the cover art: ${e.message}`));
      return;
    }
    if (!v.alive) return;
    const now = sizedArtwork(album.artwork, 300);
    const current = h('figure', { class: 'cover-now' },
      now ? h('img', { src: now, alt: 'The cover now', width: 150, height: 150 }) : h('div', { class: 'cover-none muted' }, 'No cover'),
      h('figcaption', { class: 'muted small' }, art.archive ? 'From the Cover Art Archive (not in your files)' : [
        art.embedded ? `In ${art.embedded} of ${plural(art.files, 'file')}` : null,
        art.folder_image ? `${art.embedded ? '; f' : 'F'}older: ${art.folder_image}` : null,
      ].filter(Boolean).join('') || 'Now'));
    if (art.archive) current.title = 'Shown because the album has no picture of its own. Save it to keep it with your music.';
    if (!art.release_mbid) {
      coverEl.replaceChildren(title, current,
        h('p', { class: 'muted small' }, 'Match the album to a MusicBrainz release (above) to choose cover art from the Cover Art Archive.'));
      return;
    }
    if (!art.images.length) {
      coverEl.replaceChildren(title, current,
        h('p', { class: 'muted small' }, 'The Cover Art Archive has no images of this release.'));
      return;
    }
    let chosen = art.images[0].id;
    const picks = art.images.map((im) => {
      const b = h('button', { class: `cover-pick${im.id === chosen ? ' chosen' : ''}`, type: 'button', 'aria-pressed': String(im.id === chosen),
        onclick: () => {
          chosen = im.id;
          for (const x of picks) { x.classList.toggle('chosen', x === b); x.setAttribute('aria-pressed', String(x === b)); }
        } },
      h('img', { src: im.thumbnail, alt: (im.types || []).join(', ') || 'Image', loading: 'lazy', width: 150, height: 150 }),
      h('span', { class: 'small' }, (im.types || []).join(', ') || 'Image'));
      return b;
    });
    // a folder holding other music too: a picture there would be theirs as well
    const folderBox = h('input', { type: 'checkbox', id: 'cover-folder', checked: !art.folder_shared, disabled: art.folder_shared });
    const embedBox = h('input', { type: 'checkbox', id: 'cover-embed', checked: art.embedded > 0 || art.folder_shared });
    const hint = h('p', { class: 'muted small', hidden: !(art.embedded > 0) || embedBox.checked },
      'Pictures embedded in the files are shown before one in the folder: embed it too, or the old one stays.');
    embedBox.addEventListener('change', () => { hint.hidden = !(art.embedded > 0) || embedBox.checked; });
    const go = h('button', { class: 'btn btn-primary', type: 'button', disabled: !!art.blocked, onclick: async () => {
      if (!folderBox.checked && !embedBox.checked) { toast('Choose where to put it', 'error'); return; }
      go.disabled = true;
      try {
        const r = await setCoverArt(id, { image: chosen, folder: folderBox.checked, embed: embedBox.checked }, releaseId);
        if (!v.alive) return;
        if (r.failed?.length) toast(r.failed.map((f) => f.error).join('; '), 'error');
        coverResult = r;
        loadCover();
      } catch (e) {
        toast(`Could not change the cover: ${e.message}`, 'error');
        go.disabled = false;
      }
    } }, icon('edit'), 'Use this cover');
    const done = coverResult && h('div', { class: 'notice', role: 'status' },
      h('p', {}, `Changed the cover (${plural(coverResult.changed, 'change')}).`),
      coverResult.batch != null && h('button', { class: 'btn', type: 'button', onclick: async (e) => {
        e.target.disabled = true;
        try {
          await undoTagEdit(coverResult.batch);
          toast('Put the old cover back');
          coverResult = null;
          loadCover();
        } catch (err) { toast(`Could not undo: ${err.message}`, 'error'); e.target.disabled = false; }
      } }, 'Undo'));
    coverEl.replaceChildren(...[
      title,
      h('div', { class: 'cover-row' }, current, ...picks),
      h('p', { class: 'muted small' }, 'From the Cover Art Archive. Pick one, then where it goes.'),
      h('div', { class: 'field check' }, folderBox, h('label', { for: 'cover-folder' }, 'Save it in the album’s folder (cover.jpg)',
        art.folder_shared ? h('span', { class: 'muted small block' }, 'The folder holds other music too, which would take the picture as well: embed it instead.') : null)),
      h('div', { class: 'field check' }, embedBox, h('label', { for: 'cover-embed' }, `Embed it in each file (${plural(art.files, 'file')})`)),
      hint,
      done,
      h('div', { class: 'button-row' }, go),
    ].filter(Boolean));
  }

  // ---------------------------------------------------------------- folders and file names

  async function loadFiles() {
    let plan;
    try {
      plan = await getAlbumFiles(id, template);
    } catch (e) {
      if (v.alive) filesEl.replaceChildren(h('h2', {}, 'Folders and file names'), h('p', { class: 'muted' }, e.message));
      return;
    }
    if (!v.alive) return;
    template = plan.template;
    const input = h('input', { type: 'text', value: plan.template, spellcheck: 'false', autocomplete: 'off', 'aria-label': 'Name template' });
    const preview = (e) => { e?.preventDefault(); template = input.value.trim() || null; loadFiles(); };
    const moving = plan.files.filter((f) => f.changed && !f.error);
    const tracks = moving.filter((f) => f.track_id != null).length;
    const problems = plan.files.filter((f) => f.error);
    const rowsEl = plan.files.filter((f) => f.changed || f.error).map((f) => h('tr', { class: f.error ? 'blocked' : '' },
      h('td', { class: 'tag-current' }, f.from),
      h('td', {}, f.error ? h('span', { class: 'muted' }, f.error) : f.to)));
    const go = h('button', { class: 'btn btn-primary', type: 'button', disabled: !moving.length || !!plan.blocked, onclick: async () => {
      go.disabled = true;
      try {
        const r = await moveAlbumFiles(id, template);
        if (!v.alive) return;
        if (r.failed?.length) toast(`${plural(r.failed.length, 'file')} not moved`, 'error');
        carried = { ...r, moved: true };
        if (r.album_id != null && r.album_id !== +id) {
          location.hash = `#/library/tags/${enc(r.album_id)}?name=${enc(name)}`;
        } else {
          lastResult = carried; carried = null;
          load();
        }
      } catch (e) {
        toast(`Could not move the files: ${e.message}`, 'error');
        go.disabled = false;
      }
    } }, icon('edit'), moving.length ? `Move ${plural(moving.length, 'file')}` : 'Nothing to move');
    filesEl.replaceChildren(...[
      h('h2', {}, 'Folders and file names'),
      h('p', { class: 'muted small' }, 'Where the files go in their music folder, from their tags (apply tag changes first). ',
        h('code', {}, '{album_artist} {album} {year} {disc} {track:02} {title} …'), '; a part in ',
        h('code', {}, '[ ]'), ' is left out when its tags are empty. Covers and other files in the album’s folder go along.'),
      h('form', { class: 'inline-row', onsubmit: preview }, input,
        h('button', { class: 'btn', type: 'submit' }, 'Preview'),
        h('button', { class: 'btn', type: 'button', onclick: async () => {
          try { await patchSettings({ file_name_template: input.value.trim() }); toast('Template saved for every album'); } catch (e) { toast(`Could not save: ${e.message}`, 'error'); }
        } }, 'Save as default')),
      rowsEl.length
        ? h('table', { class: 'file-table' },
          h('thead', {}, h('tr', {}, h('th', {}, 'Now'), h('th', {}, 'New'))), h('tbody', {}, rowsEl))
        : h('p', { class: 'muted small' }, 'Every file is where the template puts it.'),
      problems.length ? h('p', { class: 'muted small' }, `${plural(problems.length, 'file')} can’t move (see above).`) : null,
      h('div', { class: 'button-row' }, go,
        tracks !== moving.length && moving.length ? h('span', { class: 'muted small' }, `${plural(tracks, 'track')}, ${plural(moving.length - tracks, 'other file')}`) : null),
    ].filter(Boolean));
  }

  function render() {
    renderTop();
    rows = new Map(data.tracks.map((t) => [t.track_id, new Map()]));
    // loose tracks (no album tag) aren't an album: each on its own, album tags included
    tracksEl.replaceChildren(...[
      data.loose ? h('p', { class: 'notice' }, 'These tracks have no album tag, so they are not an album: each is changed on its own. Give them their album’s name to group them.') : renderAlbum(),
      ...data.tracks.map(renderTrack),
    ]);
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
      if ((ALBUM_FIELDS.includes(x.field) && !data.loose) || !(x.changed || showAll)) continue;
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
        toast(r.moved ? `Moved ${plural(u.changed, 'file')} back` : `Put back the tags of ${plural(u.changed, 'file')}`);
        lastResult = null;
        if (r.moved) {
          // the album may have a new id again: find it by its title
          const found = await getAlbums({ q: name, limit: 20 }).catch(() => null);
          const same = found?.items?.filter((a) => a.title === name) || [];
          location.hash = same.length === 1
            ? `#/library/tags/${enc(same[0].id)}?name=${enc(name)}`
            : `#/library/albums`;
        } else {
          setTimeout(() => v.alive && load(), 1500);
        }
      } catch (err) { toast(`Could not undo: ${err.message}`, 'error'); e.target.disabled = false; }
    } }, 'Undo');
    return h('div', { class: 'notice', role: 'status' },
      h('p', {}, r.moved ? `Moved ${plural(r.changed, 'track')}.` : `Changed the tags of ${plural(r.changed, 'file')}.`),
      r.failed?.length ? h('ul', {}, r.failed.map((f) => h('li', {}, f.error))) : null,
      undo);
  }

  await load();
}
