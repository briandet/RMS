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

import { rpc, itemArt, coverUrl, portraitUrl } from '../api.js';
import { store } from '../store.js';
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
const ALBUM_TAGS = 'tags:ljaysSKw';
const TRACK_TAGS = 'tags:aAdlntuKceis';

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
    pager(v, ul, async (start, count) => {
      const r = await rpc(['artists', start, count, 'tags:s4']);
      return { items: r.artists_loop || [], total: r.count };
    }, (a) => {
      const out = [];
      if (a.textkey && a.textkey !== lastKey) {
        lastKey = a.textkey;
        out.push(h('li', { class: 'letter', 'aria-hidden': 'true' }, a.textkey));
      }
      out.push(row({
        href: `#/library/artist/${enc(a.id)}?name=${enc(a.artist)}`,
        title: a.artist, iconName: 'person', art: portraitUrl(a.portraitid, 80) ?? undefined, cls: 'artist',
        actions: actionButtons(v.ctx, 'artist_id', a.id, a.artist, { compact: true }),
      }));
      return out;
    }, { empty: NO_MUSIC() });
  },

  async artist(v, id) {
    const r = await rpc(['artists', 0, 1, `artist_id:${id}`, 'tags:4']).catch(() => ({}));
    if (!v.alive) return;
    const a = r.artists_loop?.[0] || {};
    const name = v.query.get('name') || a.artist || 'Artist';
    v.crumbs([crumbRoot('artists'), { label: name }]);
    const countEl = h('span');
    const art = a.portraitid ? portraitUrl(a.portraitid, 400) : undefined;
    const head = pageHeader({ title: name, sub: countEl, actions: bigActions(v.ctx, 'artist_id', id, name), art, iconName: 'person' });
    head.classList.add('artist');
    v.body.append(head);
    albumsGrid(v, [`artist_id:${id}`], (total) => { countEl.textContent = plural(total, 'album'); });
  },

  // ------------------------------------------------------------ albums
  albums(v) {
    v.crumbs([crumbRoot('albums')]);
    const sorts = [['album', 'Title'], ['artflow', 'Artist'], ['new', 'Recently added']];
    let sort = storage.get('rms.albumSort', 'album');
    if (!sorts.some(([k]) => k === sort)) sort = 'album';
    const countEl = h('span', { class: 'muted' });
    const select = h('select', { 'aria-label': 'Sort albums', onchange: (e) => { storage.set('rms.albumSort', e.target.value); v.ctx.reload(); } },
      sorts.map(([k, l]) => h('option', { value: k, selected: k === sort }, l)));
    v.body.append(h('div', { class: 'toolbar' }, countEl, h('label', { class: 'sort' }, 'Sort ', select)));
    albumsGrid(v, [`sort:${sort}`], (total) => { countEl.textContent = plural(total, 'album'); }, NO_MUSIC());
  },

  async album(v, id) {
    v.crumbs([crumbRoot('albums'), { label: v.query.get('name') || 'Album' }]);
    v.body.append(loadingBox());
    const [info, tr] = await Promise.all([
      rpc(['albums', 0, 1, ALBUM_TAGS, `album_id:${id}`]).catch(() => ({})),
      rpc(['titles', 0, 500, TRACK_TAGS, `album_id:${id}`, 'sort:tracknum']),
    ]);
    if (!v.alive) return;
    const tracks = tr.titles_loop || [];
    const a = info.albums_loop?.[0] || {};
    const title = a.album || tracks[0]?.album || v.query.get('name') || 'Album';
    const artist = a.artist || tracks[0]?.artist || '';
    const artistId = a.artist_id ?? tracks[0]?.artist_id;
    const crumbs = [];
    if (artistId != null && artist) crumbs.push(crumbRoot('artists'), { label: artist, href: `#/library/artist/${enc(artistId)}?name=${enc(artist)}` });
    else crumbs.push(crumbRoot('albums'));
    crumbs.push({ label: title });
    v.crumbs(crumbs);

    const total = tracks.reduce((s, t) => s + (+t.duration || 0), 0);
    const art = itemArt(a, 600) || coverUrl(tracks[0]?.coverid, 600);
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
        number: t.tracknum || i + 1,
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
    pager(v, ul, async (start, count) => {
      const r = await rpc(['genres', start, count]);
      return { items: r.genres_loop || [], total: r.count };
    }, (g) => row({
      href: `#/library/genre/${enc(g.id)}?name=${enc(g.genre)}`, title: g.genre, iconName: 'genre',
      actions: actionButtons(v.ctx, 'genre_id', g.id, g.genre, { compact: true }),
    }), { empty: NO_MUSIC() });
  },

  genre(v, id) {
    const name = v.query.get('name') || 'Genre';
    v.crumbs([crumbRoot('genres'), { label: name }]);
    const countEl = h('span');
    v.body.append(pageHeader({ title: name, sub: countEl, actions: bigActions(v.ctx, 'genre_id', id, name) }));
    albumsGrid(v, [`genre_id:${id}`], (t) => { countEl.textContent = plural(t, 'album'); });
  },

  // ------------------------------------------------------------ years
  years(v) {
    v.crumbs([crumbRoot('years')]);
    const ul = list('Years');
    v.body.append(ul);
    pager(v, ul, async (start, count) => {
      const r = await rpc(['years', start, count]);
      return { items: r.years_loop || [], total: r.count };
    }, (y) => {
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
    albumsGrid(v, [`year:${year}`], (t) => { countEl.textContent = plural(t, 'album'); });
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
    pager(v, ul, async (start, count) => {
      const cmd = ['musicfolder', start, count, 'tags:dutcJK'];
      if (id) cmd.push(`folder_id:${id}`);
      const r = await rpc(cmd);
      return { items: r.folder_loop || [], total: r.count };
    }, (f) => {
      const name = f.filename || f.title || '—';
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
        return trackRow(v.ctx, { ...f, title: f.title || name }, {
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
    pager(v, ul, async (start, count) => {
      const r = await rpc(['playlists', start, count]);
      return { items: r.playlists_loop || [], total: r.count };
    }, (p) => row({
      href: `#/library/playlist/${enc(p.id)}?name=${enc(p.playlist)}`, title: p.playlist, iconName: 'playlist',
      actions: actionButtons(v.ctx, 'playlist_id', p.id, p.playlist, { compact: true }),
    }), { empty: 'No saved playlists.' });
  },

  async playlist(v, id) {
    const name = v.query.get('name') || 'Playlist';
    v.crumbs([crumbRoot('playlists'), { label: name }]);
    const sub = h('span');
    v.body.append(pageHeader({ title: name, sub, actions: bigActions(v.ctx, 'playlist_id', id, name), iconName: 'playlist' }));
    const ul = list(`Tracks of ${name}`);
    ul.classList.add('tracks');
    v.body.append(ul);
    let duration = 0, n = 0;
    pager(v, ul, async (start, count) => {
      const r = await rpc(['playlists', 'tracks', start, count, `playlist_id:${id}`, 'tags:adlcuK']);
      return { items: r.playlisttracks_loop || [], total: r.count };
    }, (t, i) => {
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
    const favCmd = (action, itemId, label) => async () => {
      if (!store.playerId) { toast('Select a player first', 'error'); return; }
      const r = await v.ctx.cmd('favorites', 'playlist', action, `item_id:${itemId}`);
      if (r) toast(`${{ play: 'Playing', add: 'Added to queue', insert: 'Playing next' }[action]}: ${label}`);
    };
    pager(v, ul, async (start, count) => {
      const cmd = ['favorites', 'items', start, count, 'want_url:1'];
      if (id) cmd.push(`item_id:${id}`);
      const r = await rpc(cmd);
      return { items: r.loop_loop || [], total: r.count };
    }, (f) => {
      const name = f.name || f.title || '—';
      const art = f.image ? itemArt({ image: f.image }) : undefined;
      if (+f.hasitems && !+f.isaudio) {
        trails.favorites.set(String(f.id), [...trail, { id: f.id, name }]);
        return row({ href: `#/library/favorites/${enc(f.id)}?name=${enc(name)}`, title: name, iconName: 'folder', art });
      }
      const run = (action) => favCmd(action === 'load' ? 'play' : action, f.id, name)();
      return row({
        title: name, sub: f.type === 'audio' && /^https?:/i.test(f.url || '') ? 'Stream' : null,
        iconName: /^https?:/i.test(f.url || '') ? 'radio' : 'heart', art,
        label: `Play ${name}`, onActivate: () => run('load'),
        actions: actionButtons(v.ctx, null, null, name, { compact: true, run }),
      });
    }, { empty: h('div', {}, h('p', {}, 'No favorites yet.'), h('p', { class: 'muted' }, 'Use the heart button on the Now Playing panel to save the current track or station.')) });
  },

  // ------------------------------------------------------------ spotify
  async spotify(v, id) {
    const trail = id ? trails.spotify.get(String(id)) || [{ id, name: v.query.get('name') || 'Spotify' }] : [];
    v.crumbs([crumbRoot('spotify'), ...trail.map((t) => ({ label: t.name, href: `#/library/spotify/${enc(t.id)}?name=${enc(t.name)}` }))]);
    if (!id) {
      const st = await rpc(['spotify', 'status']).catch(() => ({}));
      if (!v.alive) return;
      if (!+st.signed_in) {
        v.body.append(emptyBox(h('div', {},
          h('p', {}, 'Not signed in to Spotify.'),
          h('p', { class: 'muted' }, 'Sign in under ', h('a', { href: '#/settings' }, 'Settings → Spotify'), ' (Spotify Premium is required).'))));
        return;
      }
    }
    const ul = list('Spotify');
    v.body.append(ul);
    const spCmd = (action, itemId, label) => async () => {
      if (!store.playerId) { toast('Select a player first', 'error'); return; }
      const r = await v.ctx.cmd('spotify', 'playlist', action, `item_id:${itemId}`);
      if (r) toast(`${{ play: 'Playing', add: 'Added to queue', insert: 'Playing next' }[action]}: ${label}`);
    };
    pager(v, ul, async (start, count) => {
      const cmd = ['spotify', 'items', start, count, 'want_url:1'];
      if (id) cmd.push(`item_id:${id}`);
      const r = await rpc(cmd);
      return { items: r.loop_loop || [], total: r.count };
    }, (f) => {
      const name = f.name || f.title || '—';
      const art = f.image ? itemArt({ image: f.image }) : undefined;
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
      const run = (action) => spCmd(action === 'load' ? 'play' : action, f.id, name)();
      const playable = +f.isaudio || f.type === 'playlist';
      if (+f.hasitems && !+f.isaudio) {
        trails.spotify.set(String(f.id), [...trail, { id: f.id, name }]);
        return row({
          href: `#/library/spotify/${enc(f.id)}?name=${enc(name)}`, title: name, iconName: 'folder', art,
          actions: playable ? actionButtons(v.ctx, null, null, name, { compact: true, run }) : null,
        });
      }
      if (f.type === 'text') return row({ title: name, iconName: 'link' });
      return row({
        title: name, iconName: 'note', art, label: `Play ${name}`, onActivate: () => run('load'),
        actions: actionButtons(v.ctx, null, null, name, { compact: true, run }),
      });
    }, { empty: 'Nothing here.' });
  },
};

/** Album grid with infinite scrolling. */
function albumsGrid(v, filters, onTotal, empty = 'No albums found.') {
  const grid = albumGrid();
  v.body.append(grid);
  pager(v, grid, async (start, count) => {
    const r = await rpc(['albums', start, count, ALBUM_TAGS, ...filters]);
    return { items: r.albums_loop || [], total: r.count };
  }, (a) => albumCard(v.ctx, a), { pageSize: 60, empty, onFirst: onTotal });
}

/** Load a whole container (album/playlist/folder) and jump to one of its tracks. */
async function playFrom(ctx, key, id, index, label) {
  await ctx.playItem('load', key, id, label, { start: index > 0 ? index : undefined });
}
