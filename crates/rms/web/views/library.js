// Library browser: Artists / Albums / Genres / Years / Folders / Playlists / Favorites / Spotify
// with drill-down pages and breadcrumbs.
//
// Routes (hash):
//   #/library/<tab>                 tab = artists|albums|genres|years|folders|playlists|favorites
//   #/library/artist/<id>?name=     albums by artist
//   #/library/album/<id>?name=      album tracks
//   #/library/genre/<id>?name=      albums in genre
//   #/library/year/<year>           albums from year
//   #/library/folders/<id>?name=    music folder contents
//   #/library/playlist/<id>?name=   saved playlist tracks
//   #/library/favorites/<id>?name=  favorites sub-folder
//   #/library/spotify/<id>?name=    Spotify menu level (item id; `<n>_<term>` for a search)

import {
  getArtists, getArtist, getAlbums, getAlbum, getGenres, getYears, getFolder, getPlaylists,
  getPlaylist, getPlaylistTracks, getFavorites, getSpotify, getSpotifyItems, sizedArtwork,
} from '../api.js';
import { h, fmtDuration, plural, storage, toast } from '../util.js';
import {
  enc, pager, list, row, trackRow, albumCard, albumGrid, actionButtons, bigActions,
  pageHeader, emptyBox, errorBox, loadingBox,
} from './common.js';

const TABS = [
  ['artists', 'Artists'], ['albums', 'Albums'], ['genres', 'Genres'], ['years', 'Years'],
  ['folders', 'Folders'], ['playlists', 'Playlists'], ['favorites', 'Favorites'], ['spotify', 'Spotify'],
];
const TAB_OF = { artist: 'artists', album: 'albums', genre: 'genres', year: 'years', playlist: 'playlists' };

const NO_MUSIC = () => h('div', {}, h('p', {}, 'Your library is empty.'),
  h('p', { class: 'muted' }, 'Add a music folder in ', h('a', { href: '#/settings' }, 'Settings'), ' and run a rescan.'));

// Remember breadcrumb trails for hierarchical browsing (folders, favorites).
const trails = { folders: new Map(), favorites: new Map(), spotify: new Map() };

export function render(node, { parts, query, ctx, scrollRoot }) {
  const section = parts[1] || 'albums';
  const id = parts[2];
  const activeTab = TAB_OF[section] || section;

  const crumbs = h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb' });
  const body = h('div', { class: 'lib-body' });
  node.append(tabsNav(activeTab), crumbs, body);

  const v = {
    ctx, query, body, scrollRoot, alive: true, cleanup: [],
    crumbs(items) {
      crumbs.hidden = items.length < 2; // top-level tabs already show where you are
      crumbs.replaceChildren(h('ol', {}, items.map((c, i) => h('li', {},
        c.href && i < items.length - 1 ? h('a', { href: c.href }, c.label) : h('span', { 'aria-current': 'page' }, c.label)))));
      document.title = `${items[items.length - 1]?.label || 'Library'} · RMS`;
    },
  };

  const handler = ROUTES[section] || ROUTES.albums;
  Promise.resolve(handler(v, id)).catch((e) => {
    if (!v.alive) return;
    console.error(e);
    body.replaceChildren(errorBox(e, () => ctx.reload()));
  });

  return {
    cleanup() { v.alive = false; v.cleanup.forEach((f) => f()); },
    onShow() { document.title = `${crumbs.querySelector('[aria-current]')?.textContent || 'Library'} · RMS`; },
  };
}

function tabsNav(active) {
  return h('nav', { class: 'tabs', 'aria-label': 'Library sections' },
    TABS.map(([k, label]) => h('a', { href: `#/library/${k}`, class: 'tab', 'aria-current': k === active ? 'page' : null }, label)));
}

const crumbRoot = (tab) => ({ label: TABS.find(([k]) => k === tab)?.[1] || 'Library', href: `#/library/${tab}` });

const ROUTES = {
  // ------------------------------------------------------------ artists
  artists(v) {
    v.crumbs([crumbRoot('artists')]);
    const ul = list('Artists');
    v.body.append(ul);
    let lastKey = null;
    pager(v, ul, (offset, limit) => getArtists({ offset, limit }), (a) => {
      const out = [];
      if (a.letter && a.letter !== lastKey) {
        lastKey = a.letter;
        out.push(h('li', { class: 'letter', 'aria-hidden': 'true' }, a.letter));
      }
      out.push(row({
        href: `#/library/artist/${enc(a.id)}?name=${enc(a.name)}`,
        title: a.name, iconName: 'person', art: sizedArtwork(a.portrait, 80) ?? undefined, cls: 'artist',
        actions: actionButtons(v.ctx, 'artist_id', a.id, a.name, { compact: true }),
      }));
      return out;
    }, { empty: NO_MUSIC() });
  },

  async artist(v, id) {
    const a = await getArtist(id).catch(() => ({}));
    if (!v.alive) return;
    const name = v.query.get('name') || a.name || 'Artist';
    v.crumbs([crumbRoot('artists'), { label: name }]);
    const countEl = h('span');
    const art = a.portrait ? sizedArtwork(a.portrait, 400) : undefined;
    const head = pageHeader({ title: name, sub: countEl, actions: bigActions(v.ctx, 'artist_id', id, name), art, iconName: 'person' });
    head.classList.add('artist');
    v.body.append(head);
    albumsGrid(v, { artist: id }, (total) => { countEl.textContent = plural(total, 'album'); });
  },

  // ------------------------------------------------------------ albums
  albums(v) {
    v.crumbs([crumbRoot('albums')]);
    const sorts = [['title', 'Title'], ['artist', 'Artist'], ['added', 'Recently added']];
    let sort = storage.get('rms.albumSort', 'title');
    if (!sorts.some(([k]) => k === sort)) sort = 'title';
    const countEl = h('span', { class: 'muted' });
    const select = h('select', { 'aria-label': 'Sort albums', onchange: (e) => { storage.set('rms.albumSort', e.target.value); v.ctx.reload(); } },
      sorts.map(([k, l]) => h('option', { value: k, selected: k === sort }, l)));
    v.body.append(h('div', { class: 'toolbar' }, countEl, h('label', { class: 'sort' }, 'Sort ', select)));
    albumsGrid(v, { sort }, (total) => { countEl.textContent = plural(total, 'album'); }, NO_MUSIC());
  },

  async album(v, id) {
    v.crumbs([crumbRoot('albums'), { label: v.query.get('name') || 'Album' }]);
    v.body.append(loadingBox());
    const a = await getAlbum(id);
    if (!v.alive) return;
    const tracks = a.tracks || [];
    const title = a.title || tracks[0]?.album || v.query.get('name') || 'Album';
    const artist = a.artist || tracks[0]?.artist || '';
    const artistId = a.artist_id ?? tracks[0]?.artist_id;
    const crumbs = [];
    if (artistId != null && artist) crumbs.push(crumbRoot('artists'), { label: artist, href: `#/library/artist/${enc(artistId)}?name=${enc(artist)}` });
    else crumbs.push(crumbRoot('albums'));
    crumbs.push({ label: title });
    v.crumbs(crumbs);

    const total = a.duration || tracks.reduce((s, t) => s + (+t.duration || 0), 0);
    const art = sizedArtwork(a.artwork || tracks.find((t) => t.artwork)?.artwork, 600);
    const sub = h('div', {},
      artist && (artistId != null
        ? h('a', { href: `#/library/artist/${enc(artistId)}?name=${enc(artist)}` }, artist)
        : artist),
      h('div', { class: 'muted' }, [+a.year ? a.year : null, plural(tracks.length, 'track'), total ? fmtDuration(total) : null].filter(Boolean).join(' · ')));

    const ul = list(`Tracks of ${title}`);
    ul.classList.add('tracks');
    const discs = new Set(tracks.map((t) => t.disc).filter(Boolean));
    let lastDisc = null;
    tracks.forEach((t, i) => {
      if (discs.size > 1 && t.disc !== lastDisc) {
        lastDisc = t.disc;
        ul.append(h('li', { class: 'letter' }, `Disc ${t.disc}`));
      }
      ul.append(trackRow(v.ctx, t, {
        number: t.track_number || i + 1,
        showArtist: t.artist && t.artist !== artist,
        onPlay: () => playFrom(v.ctx, 'album_id', id, i, t.title),
      }));
    });
    v.body.replaceChildren(
      pageHeader({ title, sub, art, actions: bigActions(v.ctx, 'album_id', id, title) }),
      tracks.length ? ul : emptyBox('This album has no tracks.'));
  },

  // ------------------------------------------------------------ genres
  genres(v) {
    v.crumbs([crumbRoot('genres')]);
    const ul = list('Genres');
    v.body.append(ul);
    pager(v, ul, (offset, limit) => getGenres({ offset, limit }), (g) => row({
      href: `#/library/genre/${enc(g.id)}?name=${enc(g.name)}`, title: g.name, iconName: 'genre',
      actions: actionButtons(v.ctx, 'genre_id', g.id, g.name, { compact: true }),
    }), { empty: NO_MUSIC() });
  },

  genre(v, id) {
    const name = v.query.get('name') || 'Genre';
    v.crumbs([crumbRoot('genres'), { label: name }]);
    const countEl = h('span');
    v.body.append(pageHeader({ title: name, sub: countEl, actions: bigActions(v.ctx, 'genre_id', id, name) }));
    albumsGrid(v, { genre: id }, (t) => { countEl.textContent = plural(t, 'album'); });
  },

  // ------------------------------------------------------------ years
  years(v) {
    v.crumbs([crumbRoot('years')]);
    const ul = list('Years');
    v.body.append(ul);
    pager(v, ul, (offset, limit) => getYears({ offset, limit }), (y) => {
      const label = +y.year ? String(y.year) : 'Unknown year';
      return row({
        href: `#/library/year/${enc(y.year)}`, title: label, iconName: 'calendar',
        actions: actionButtons(v.ctx, 'year', y.year, label, { compact: true }),
      });
    }, { empty: NO_MUSIC() });
  },

  year(v, year) {
    const label = +year ? String(year) : 'Unknown year';
    v.crumbs([crumbRoot('years'), { label }]);
    const countEl = h('span');
    v.body.append(pageHeader({ title: label, sub: countEl, actions: bigActions(v.ctx, 'year', year, label) }));
    albumsGrid(v, { year }, (t) => { countEl.textContent = plural(t, 'album'); });
  },

  // ------------------------------------------------------------ folders
  folders(v, id) {
    const trail = id ? trails.folders.get(String(id)) || [{ id, name: v.query.get('name') || 'Folder' }] : [];
    v.crumbs([crumbRoot('folders'), ...trail.map((t) => ({ label: t.name, href: `#/library/folders/${enc(t.id)}?name=${enc(t.name)}` }))]);
    if (id) {
      const name = trail[trail.length - 1].name;
      v.body.append(pageHeader({ title: name, actions: bigActions(v.ctx, 'folder_id', id, name), iconName: 'folder' }));
    }
    const ul = list('Folder contents');
    v.body.append(ul);
    const items = [];
    let subfolders = false;
    pager(v, ul, (offset, limit) => getFolder(id, { offset, limit }), (f) => {
      const name = f.name || '—';
      if (f.type === 'folder') {
        subfolders = true;
        trails.folders.set(String(f.id), [...trail, { id: f.id, name }]);
        return row({
          href: `#/library/folders/${enc(f.id)}?name=${enc(name)}`, title: name, iconName: 'folder',
          actions: actionButtons(v.ctx, 'folder_id', f.id, name, { compact: true }),
        });
      }
      if (f.type === 'track') {
        const idx = items.push(f) - 1;
        return trackRow(v.ctx, { ...f.track, id: f.id, title: f.track?.title || name }, {
          showArtist: false,
          // Inside a flat folder, queue the whole folder and start at this track.
          onPlay: () => (id && !subfolders
            ? playFrom(v.ctx, 'folder_id', id, idx, name)
            : v.ctx.playItem('load', 'track_id', f.id, name)),
        });
      }
      if (f.type === 'playlist') {
        return row({
          title: name, iconName: 'playlist', label: `Play playlist ${name}`,
          onActivate: () => v.ctx.playItem('load', 'playlist_id', f.id, name),
          actions: actionButtons(v.ctx, 'playlist_id', f.id, name, { compact: true }),
        });
      }
      return row({ title: name, iconName: 'note', sub: 'Unsupported file', cls: 'disabled', onActivate: () => toast('This file type cannot be played') });
    }, { empty: id ? 'This folder is empty.' : h('div', {}, h('p', {}, 'No music folders configured.'), h('p', { class: 'muted' }, 'Add one in ', h('a', { href: '#/settings' }, 'Settings'), '.')) });
  },

  // ------------------------------------------------------------ playlists
  playlists(v) {
    v.crumbs([crumbRoot('playlists')]);
    const ul = list('Playlists');
    v.body.append(ul);
    pager(v, ul, (offset, limit) => getPlaylists({ offset, limit }), (p) => row({
      href: `#/library/playlist/${enc(p.id)}?name=${enc(p.name)}`, title: p.name, iconName: 'playlist',
      actions: actionButtons(v.ctx, 'playlist_id', p.id, p.name, { compact: true }),
    }), { empty: 'No saved playlists.' });
  },

  async playlist(v, id) {
    const name = v.query.get('name') || (await getPlaylist(id).catch(() => null))?.name || 'Playlist';
    if (!v.alive) return;
    v.crumbs([crumbRoot('playlists'), { label: name }]);
    const sub = h('span');
    v.body.append(pageHeader({ title: name, sub, actions: bigActions(v.ctx, 'playlist_id', id, name), iconName: 'playlist' }));
    const ul = list(`Tracks of ${name}`);
    ul.classList.add('tracks');
    v.body.append(ul);
    let duration = 0, n = 0;
    pager(v, ul, (offset, limit) => getPlaylistTracks(id, { offset, limit }), (t, i) => {
      duration += +t.duration || 0;
      n++;
      sub.textContent = `${plural(n, 'track')}${duration ? ' · ' + fmtDuration(duration) : ''}`;
      return trackRow(v.ctx, t, { showAlbum: true, onPlay: () => playFrom(v.ctx, 'playlist_id', id, i, t.title) });
    }, { empty: 'This playlist is empty.' });
  },

  // ------------------------------------------------------------ favorites
  favorites(v, id) {
    const trail = id ? trails.favorites.get(String(id)) || [{ id, name: v.query.get('name') || 'Folder' }] : [];
    v.crumbs([crumbRoot('favorites'), ...trail.map((t) => ({ label: t.name, href: `#/library/favorites/${enc(t.id)}?name=${enc(t.name)}` }))]);
    const ul = list('Favorites');
    v.body.append(ul);
    pager(v, ul, (offset, limit) => getFavorites(id, { offset, limit }), (f) => {
      const name = f.name || '—';
      const art = f.artwork ? sizedArtwork(f.artwork, 80) : undefined;
      const actions = f.playable ? actionButtons(v.ctx, 'favorite_id', f.id, name, { compact: true }) : null;
      if (f.type === 'folder') {
        trails.favorites.set(String(f.id), [...trail, { id: f.id, name }]);
        return row({ href: `#/library/favorites/${enc(f.id)}?name=${enc(name)}`, title: name, iconName: 'folder', art, actions });
      }
      if (!f.playable) return row({ title: name, iconName: 'link', art });
      const stream = /^https?:/i.test(f.url || '');
      return row({
        title: name, sub: stream ? 'Stream' : null,
        iconName: stream ? 'radio' : 'heart', art,
        label: `Play ${name}`, onActivate: () => v.ctx.playItem('load', 'favorite_id', f.id, name),
        actions,
      });
    }, { empty: h('div', {}, h('p', {}, 'No favorites yet.'), h('p', { class: 'muted' }, 'Use the heart button on the Now Playing panel to save the current track or station.')) });
  },

  // ------------------------------------------------------------ spotify
  async spotify(v, id) {
    const trail = id ? trails.spotify.get(String(id)) || [{ id, name: v.query.get('name') || 'Spotify' }] : [];
    v.crumbs([crumbRoot('spotify'), ...trail.map((t) => ({ label: t.name, href: `#/library/spotify/${enc(t.id)}?name=${enc(t.name)}` }))]);
    if (!id) {
      let st;
      try { st = await getSpotify(); } catch (e) {
        if (e.status !== 404) throw e;
        st = null;
      }
      if (!v.alive) return;
      if (!st) {
        v.body.append(emptyBox('Spotify is not available on this server.'));
        return;
      }
      if (!st.signed_in) {
        v.body.append(emptyBox(h('div', {},
          h('p', {}, 'Not signed in to Spotify.'),
          h('p', { class: 'muted' }, 'Sign in under ', h('a', { href: '#/settings' }, 'Settings → Spotify'), ' (Spotify Premium is required).'))));
        return;
      }
    }
    const ul = list('Spotify');
    v.body.append(ul);
    pager(v, ul, (offset, limit) => getSpotifyItems(id, { offset, limit }), (f) => {
      const name = f.name || '—';
      const art = f.artwork ? sizedArtwork(f.artwork, 80) : undefined;
      if (f.type === 'search') {
        const input = h('input', { type: 'search', placeholder: 'Search Spotify', 'aria-label': 'Search Spotify', enterkeyhint: 'search' });
        const form = h('form', { class: 'inline-row sp-search', onsubmit: (e) => {
          e.preventDefault();
          const term = input.value.trim();
          if (!term) return;
          const sid = `${f.id}_${encodeURIComponent(term)}`;
          const label = `Search: ${term}`;
          trails.spotify.set(sid, [...trail, { id: sid, name: label }]);
          location.hash = `#/library/spotify/${enc(sid)}?name=${enc(label)}`;
        } }, input, h('button', { class: 'btn', type: 'submit' }, 'Search'));
        return h('li', { class: 'row' }, form);
      }
      const actions = f.playable ? actionButtons(v.ctx, 'spotify_id', f.id, name, { compact: true }) : null;
      if (f.type === 'folder') {
        trails.spotify.set(String(f.id), [...trail, { id: f.id, name }]);
        return row({ href: `#/library/spotify/${enc(f.id)}?name=${enc(name)}`, title: name, iconName: 'folder', art, actions });
      }
      if (f.type === 'text' || !f.playable) return row({ title: name, iconName: 'link' });
      return row({
        title: name, iconName: 'note', art, label: `Play ${name}`,
        onActivate: () => v.ctx.playItem('load', 'spotify_id', f.id, name),
        actions,
      });
    }, { empty: 'Nothing here.' });
  },
};

/** Album grid with infinite scrolling. */
function albumsGrid(v, filters, onTotal, empty = 'No albums found.') {
  const grid = albumGrid();
  v.body.append(grid);
  pager(v, grid, (offset, limit) => getAlbums({ ...filters, offset, limit }),
    (a) => albumCard(v.ctx, a), { pageSize: 60, empty, onFirst: onTotal });
}

/** Load a whole container (album/playlist/folder) and jump to one of its tracks. */
async function playFrom(ctx, key, id, index, label) {
  await ctx.playItem('load', key, id, label, { start: index > 0 ? index : undefined });
}
