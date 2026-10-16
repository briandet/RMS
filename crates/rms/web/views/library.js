// Library browser: Artists / Albums / Genres / Years / Folders / Playlists / Favorites / Radio /
// Spotify
// with drill-down pages and breadcrumbs.
//
// Routes (hash):
//   #/library/<tab>                 tab = artists|albums|genres|years|folders|playlists|favorites
//   #/library/artist/<id>?name=     albums by artist
//   #/library/album/<id>?name=      album tracks
//   #/library/tags/<id>?name=       the album's tags and its MusicBrainz release
//   #/library/track/<id>?name=      what the analyses made of a track
//   #/library/genre/<id>?name=      albums in genre
//   #/library/year/<year>           albums from year
//   #/library/folders/<id>?name=    music folder contents
//   #/library/playlist/<id>?name=   saved playlist tracks
//   #/library/favorites/<id>?name=  favorites sub-folder
//   #/library/radio/<id>?name=      radio provider / menu level (`radiobrowser.0_<term>` for a search)
//   #/library/spotify/<id>?name=    Spotify menu level (item id; `<n>_<term>` for a search)
//   #/library/spotify-learned       the Spotify tracks learned for mixes, as a table
//   #/library/listenbrainz/<id>?name=  recommendations / a playlist made for you

import { tagsPage, releaseLabel } from './tags.js';
import { trackPage } from './track.js';
import { render as tablePage } from './table.js';
import { learnedPage } from './learned.js';
import {
  getAlbumRelease, getArtists, getArtist, getAlbums, getAlbum, getGenres, getYears, getFolder, getPlaylists,
  getPlaylist, getPlaylistTracks, getFavorites, getRadio, getSpotify, getSpotifyItems, sizedArtwork,
  getListenBrainz, getArtistTopTracks, getSimilarArtists, createPlaylist, renamePlaylist,
  deletePlaylist, reorderPlaylist,
} from '../api.js';
import { withMenu, itemEntries, askName } from './menu.js';
import { sortable, moved } from './sortable.js';
import { h, icon, fmtDuration, plural, storage, toast } from '../util.js';
import {
  enc, pager, list, row, trackRow, albumCard, albumGrid, actionButtons, bigActions,
  pageHeader, emptyBox, errorBox, loadingBox, lazyArt,
} from './common.js';

const TABS = [
  ['artists', 'Artists'], ['albums', 'Albums'], ['genres', 'Genres'], ['years', 'Years'],
  ['folders', 'Folders'], ['table', 'Table'], ['playlists', 'Playlists'], ['favorites', 'Favorites'], ['radio', 'Radio'],
  ['spotify', 'Spotify'], ['listenbrainz', 'ListenBrainz'],
];
/** How an artist's albums are grouped on their page, in order. */
const RELEASE_GROUPS = [
  ['album', 'Albums'], ['ep', 'EPs'], ['single', 'Singles'], ['live', 'Live'],
  ['compilation', 'Compilations'], ['other', 'Other releases'], ['appears', 'Appears on'],
];

/** The group of an album on artist `artistId`'s page. */
function releaseGroup(al, artistId) {
  if (al.artist_id !== artistId) return 'appears';
  const t = (al.release_type || 'album').toLowerCase();
  if (RELEASE_GROUPS.some(([k]) => k === t)) return t;
  return 'other';
}

const TAB_OF = { 'spotify-learned': 'spotify', artist: 'artists', album: 'albums', tags: 'albums', track: 'albums', genre: 'genres', year: 'years', playlist: 'playlists' };

const NO_MUSIC = () => h('div', {}, h('p', {}, 'Your library is empty.'),
  h('p', { class: 'muted' }, 'Add a music folder in ', h('a', { href: '#/settings' }, 'Settings'), ' and run a rescan.'));

// Remember breadcrumb trails for hierarchical browsing (folders, favorites).
const trails = { folders: new Map(), favorites: new Map(), radio: new Map(), spotify: new Map(), listenbrainz: new Map() };

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
      const actions = actionButtons(v.ctx, 'artist_id', a.id, a.name, { compact: true });
      out.push(withMenu(row({
        href: `#/library/artist/${enc(a.id)}?name=${enc(a.name)}`,
        title: a.name, iconName: 'person', art: sizedArtwork(a.portrait, 80) ?? undefined, cls: 'artist',
        actions,
      }), () => [a.name, itemEntries(v.ctx, 'artist_id', a.id, a.name)], actions));
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
    // sections in a fixed order, filled as they arrive
    const top = h('section', { class: 'artist-section', hidden: true });
    const releases = h('div', {}, loadingBox());
    const similar = h('section', { class: 'artist-section', hidden: true });
    v.body.append(head, top, releases, similar);

    getArtistTopTracks(id).then((tracks) => {
      if (!v.alive || !tracks.length) return;
      const ul = list(`Most played by ${name}`);
      ul.classList.add('tracks');
      tracks.forEach((t) => ul.append(trackRow(v.ctx, t, { showArtist: false, showAlbum: true })));
      top.replaceChildren(h('h2', { class: 'section-title' }, 'Most played'), ul);
      top.hidden = false;
    }).catch(() => {});

    getAlbums({ artist: id, limit: 500 }).then(({ items, total }) => {
      if (!v.alive) return;
      const groups = new Map(RELEASE_GROUPS.map(([k, label]) => [k, { label, albums: [] }]));
      for (const al of items) groups.get(releaseGroup(al, +id)).albums.push(al);
      const own = items.filter((al) => releaseGroup(al, +id) !== 'appears').length;
      const appears = items.length - own;
      countEl.textContent = [own && plural(own, 'release'), appears && `on ${plural(appears, 'other album')}`].filter(Boolean).join(' · ')
        || plural(total, 'album');
      const out = [];
      for (const { label, albums } of groups.values()) {
        if (!albums.length) continue;
        const grid = albumGrid();
        albums.sort((x, y) => (+y.year || 0) - (+x.year || 0)).forEach((al) => grid.append(albumCard(v.ctx, al)));
        out.push(h('section', { class: 'artist-section' }, h('h2', { class: 'section-title' }, label), grid));
      }
      releases.replaceChildren(...(out.length ? out : [emptyBox('No albums.')]));
    }).catch((e) => { if (v.alive) releases.replaceChildren(errorBox(e, () => v.ctx.reload())); });

    getSimilarArtists(id).then((found) => {
      if (!v.alive || !found.length) return;
      similar.replaceChildren(h('h2', { class: 'section-title' }, 'Similar artists'),
        h('ul', { class: 'chips', 'aria-label': `Artists similar to ${name}` }, ...found.map((s) => h('li', {},
          s.artist
            ? h('a', { class: 'chip', href: `#/library/artist/${enc(s.artist.id)}?name=${enc(s.artist.name)}` },
              lazyArt(s.artist.portrait ? sizedArtwork(s.artist.portrait, 80) : undefined, { cls: 'chip-art', fallback: 'person' }), s.name)
            : h('span', { class: 'chip off', title: 'Not in your library' }, s.name)))),
        h('p', { class: 'muted small' }, 'From ListenBrainz: artists its listeners play alongside this one.'));
      similar.hidden = false;
    }).catch(() => {});
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
    const tagsLink = h('a', { class: 'btn', href: `#/library/tags/${enc(id)}?name=${enc(title)}` }, icon('edit'), 'Tags');
    const release = h('div', { class: 'muted small' });
    v.body.replaceChildren(
      pageHeader({ title, sub: h('div', {}, sub, release), art, actions: [...bigActions(v.ctx, 'album_id', id, title), tagsLink] }),
      tracks.length ? ul : emptyBox('This album has no tracks.'));
    // the MusicBrainz release, when matched
    getAlbumRelease(id).then((r) => {
      if (!v.alive || !r.release) return;
      release.append('MusicBrainz: ', h('a', { href: r.release.url, target: '_blank', rel: 'noopener' }, releaseLabel(r.release)),
        r.status === 'uncertain' ? ' (uncertain)' : '');
    }).catch(() => {});
  },

  tags(v, id) {
    return tagsPage(v, id);
  },

  track(v, id) {
    return trackPage(v, id);
  },

  table(v) {
    return tablePage(v);
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
    v.body.append(h('div', { class: 'toolbar' }, h('span'),
      h('button', { class: 'btn', type: 'button', onclick: async () => {
        const name = await askName('New playlist', '', 'Create');
        if (!name) return;
        try {
          const p = await createPlaylist({ name });
          location.hash = `#/library/playlist/${enc(p.id)}?name=${enc(p.name)}`;
        } catch (e) { toast(`Could not make the playlist: ${e.message}`, 'error'); }
      } }, icon('add'), 'New playlist')));
    const ul = list('Playlists');
    v.body.append(ul);
    pager(v, ul, (offset, limit) => getPlaylists({ offset, limit }), (p) => {
      const actions = actionButtons(v.ctx, 'playlist_id', p.id, p.name, { compact: true });
      return withMenu(row({
        href: `#/library/playlist/${enc(p.id)}?name=${enc(p.name)}`, title: p.name, iconName: 'playlist', actions,
      }), () => [p.name, itemEntries(v.ctx, 'playlist_id', p.id, p.name, { extra: playlistEntries(v, p) })], actions);
    }, { empty: h('div', {}, h('p', {}, 'No playlists yet.'),
      h('p', { class: 'muted' }, 'Make one here, save the queue (Save, above the queue), or use “Add to playlist…” in the ⋯ menu of any track or album.')) });
  },

  async playlist(v, id) {
    const p = await getPlaylist(id).catch(() => null);
    if (!v.alive) return;
    const name = p?.name || v.query.get('name') || 'Playlist';
    const editable = !!p?.editable;
    v.crumbs([crumbRoot('playlists'), { label: name }]);
    const sub = h('span');
    const actions = bigActions(v.ctx, 'playlist_id', id, name);
    if (editable) {
      actions.push(h('button', { class: 'icon-btn', type: 'button', title: 'More', 'aria-label': `More options for ${name}`, 'aria-haspopup': 'menu',
        onclick: (e) => openPlaylistMenu(v, p, e.currentTarget) }, icon('more')));
    }
    v.body.append(pageHeader({ title: name, sub, actions, iconName: 'playlist' }));
    const ul = list(`Tracks of ${name}`);
    ul.classList.add('tracks');
    v.body.append(ul);
    // `total`: the playlist's tracks (all of them, not just those loaded yet)
    let duration = 0, n = 0, total = 0;
    const showCount = () => { sub.textContent = `${plural(n, 'track')}${duration ? ' · ' + fmtDuration(duration) : ''}`; };
    const indexOf = (li) => [...ul.children].indexOf(li);
    // changes go to the server one after another, each from the order the one before left
    let saving = Promise.resolve();
    const save = (order, what, done) => {
      saving = saving.then(() => reorderPlaylist(id, order)).then(() => done?.(), (e) => {
        toast(`Could not ${what}: ${e.message}`, 'error');
        v.ctx.reload();
      });
    };
    pager(v, ul, (offset, limit) => getPlaylistTracks(id, { offset, limit }), (t) => {
      duration += +t.duration || 0;
      n++;
      showCount();
      let li = null;
      const remove = () => {
        const i = indexOf(li);
        if (i < 0) return;
        const order = Array.from({ length: total }, (_, k) => k).filter((k) => k !== i);
        total--; n--; duration -= +t.duration || 0;
        li.remove();
        showCount();
        save(order, 'remove the track', () => toast(`Removed ${t.title || 'the track'}`));
      };
      const menuExtra = editable ? [{ label: 'Remove from playlist', icon: 'close', danger: true, run: remove }] : [];
      li = trackRow(v.ctx, t, { showAlbum: true, onPlay: () => playFrom(v.ctx, 'playlist_id', id, indexOf(li), t.title), menuExtra });
      if (editable) {
        li.prepend(h('button', { class: 'icon-btn drag-handle', type: 'button', title: 'Drag to reorder',
          'aria-label': `Move ${t.title || 'track'}. Use the up and down arrow keys to reorder.` }, icon('drag')));
      }
      return li;
    }, {
      empty: editable ? 'This playlist is empty. Add to it with “Add to playlist…” in the ⋯ menu of any track or album.' : 'This playlist is empty.',
      onFirst: (t) => { total = t; },
    });
    if (editable) {
      ul.classList.add('sortable');
      sortable(ul, {
        onDrop: (from, to) => {
          if (to === from) return;
          const rows = [...ul.children];
          if (to > from) rows[to].after(rows[from]); else rows[to].before(rows[from]);
          save(moved(total, from, to), 'move the track');
        },
      });
    }
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

  // ------------------------------------------------------------ ListenBrainz
  listenbrainz(v, id) {
    serviceMenu(v, id, {
      tab: 'listenbrainz', title: 'ListenBrainz', key: 'listenbrainz_id', fetch: getListenBrainz,
      leafIcon: 'note',
      empty: id ? 'Nothing here.' : h('div', {},
        h('p', {}, 'No ListenBrainz account.'),
        h('p', { class: 'muted' }, 'Add one under ', h('a', { href: '#/settings' }, 'Settings → ListenBrainz'),
          ' to see what ListenBrainz recommends for you and the playlists it makes for you. They play from your library, or from Spotify when signed in.')),
    });
  },

  // ------------------------------------------------------------ radio
  radio(v, id) {
    serviceMenu(v, id, {
      tab: 'radio', title: 'Radio', key: 'radio_id', fetch: getRadio,
      search: 'Search stations', leafIcon: 'radio', empty: 'No stations here.',
    });
  },

  // ------------------------------------------------------------ spotify
  'spotify-learned'(v) {
    v.crumbs([crumbRoot('spotify'), { label: 'Learned tracks' }]);
    return learnedPage(v);
  },

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
      if (st.learned) {
        v.body.append(h('p', { class: 'lt-link' }, h('a', { href: '#/library/spotify-learned' },
          icon('columns'), `${plural(st.learned, 'track')} learned for mixes`)));
      }
    }
    serviceMenu(v, id, {
      tab: 'spotify', title: 'Spotify', key: 'spotify_id', fetch: getSpotifyItems,
      search: 'Search Spotify', leafIcon: 'note', empty: 'Nothing here.', crumbsDone: true,
    });
  },
};

/** A service's menu level (Radio, Spotify): folders to open, search boxes, things to play. */
function serviceMenu(v, id, { tab, title, key, fetch, search, leafIcon, empty, crumbsDone }) {
  const trail = id ? trails[tab].get(String(id)) || [{ id, name: v.query.get('name') || title }] : [];
  const href = (i, name) => `#/library/${tab}/${enc(i)}?name=${enc(name)}`;
  if (!crumbsDone) v.crumbs([crumbRoot(tab), ...trail.map((t) => ({ label: t.name, href: href(t.id, t.name) }))]);
  const ul = list(title);
  v.body.append(ul);
  pager(v, ul, (offset, limit) => fetch(id, { offset, limit }), (f) => {
    const name = f.name || '—';
    const art = f.artwork ? sizedArtwork(f.artwork, 80) : undefined;
    if (f.type === 'search') {
      const input = h('input', { type: 'search', placeholder: search, 'aria-label': search, enterkeyhint: 'search' });
      const form = h('form', { class: 'inline-row sp-search', onsubmit: (e) => {
        e.preventDefault();
        const term = input.value.trim();
        if (!term) return;
        const sid = `${f.id}_${encodeURIComponent(term)}`;
        const label = `Search: ${term}`;
        trails[tab].set(sid, [...trail, { id: sid, name: label }]);
        location.hash = href(sid, label);
      } }, input, h('button', { class: 'btn', type: 'submit' }, 'Search'));
      return h('li', { class: 'row' }, form);
    }
    const actions = f.playable ? actionButtons(v.ctx, key, f.id, name, { compact: true }) : null;
    if (f.type === 'folder') {
      trails[tab].set(String(f.id), [...trail, { id: f.id, name }]);
      return row({ href: href(f.id, name), title: name, iconName: 'folder', art, actions });
    }
    if (f.type === 'text' || !f.playable) return row({ title: name, iconName: 'link' });
    return row({
      title: name, iconName: leafIcon, art, label: `Play ${name}`,
      onActivate: () => v.ctx.playItem('load', key, f.id, name),
      actions,
    });
  }, { empty });
}

/** Album grid with infinite scrolling. */
function albumsGrid(v, filters, onTotal, empty = 'No albums found.') {
  const grid = albumGrid();
  v.body.append(grid);
  pager(v, grid, (offset, limit) => getAlbums({ ...filters, offset, limit }),
    (a) => albumCard(v.ctx, a), { pageSize: 60, empty, onFirst: onTotal });
}

/** Load a whole container (album/playlist/folder) and jump to one of its tracks. */
// ------------------------------------------------------------ playlist editing

/** Rename / delete entries of an editable playlist's menu. */
function playlistEntries(v, p) {
  if (!p.editable) return [];
  return [
    { label: 'Rename…', icon: 'edit', run: async () => {
      const name = await askName('Rename playlist', p.name);
      if (!name || name === p.name) return;
      try {
        await renamePlaylist(p.id, name);
        toast(`Renamed to ${name}`);
        location.hash.includes('/playlist/') ? (location.hash = `#/library/playlist/${enc(p.id)}?name=${enc(name)}`) : v.ctx.reload();
      } catch (e) { toast(`Could not rename: ${e.message}`, 'error'); }
    } },
    { label: 'Delete playlist', icon: 'trash', danger: true, run: async () => {
      if (!confirm(`Delete the playlist “${p.name}”?`)) return;
      try {
        await deletePlaylist(p.id);
        toast(`Deleted ${p.name}`);
        if (location.hash.includes('/playlist/')) location.hash = '#/library/playlists'; else v.ctx.reload();
      } catch (e) { toast(`Could not delete: ${e.message}`, 'error'); }
    } },
  ];
}

async function openPlaylistMenu(v, p, anchor) {
  const { openMenu } = await import('./menu.js');
  openMenu(anchor, p.name, playlistEntries(v, p));
}

async function playFrom(ctx, key, id, index, label) {
  await ctx.playItem('load', key, id, label, { start: index > 0 ? index : undefined });
}
