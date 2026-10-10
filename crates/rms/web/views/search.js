// Search results: #/search?q=term

import { rpc } from '../api.js';
import { h, plural } from '../util.js';
import { enc, list, row, trackRow, albumCard, albumGrid, actionButtons, emptyBox, errorBox, loadingBox } from './common.js';

export function render(node, { query, ctx }) {
  const q = (query.get('q') || '').trim();
  const input = document.getElementById('search-input');
  if (input && document.activeElement !== input && input.value !== q) input.value = q;
  document.title = q ? `Search: ${q} · RMS` : 'Search · RMS';

  const heading = h('h1', { class: 'view-title' }, q ? `Results for “${q}”` : 'Search');
  const body = h('div', { class: 'search-results' }, loadingBox());
  node.append(heading, body);
  let alive = true;

  if (q.length < 2) {
    body.replaceChildren(emptyBox('Type at least two characters in the search box to find artists, albums and tracks.'));
    return () => {};
  }

  Promise.allSettled([
    rpc(['search', 0, 20, `term:${q}`]),
    rpc(['albums', 0, 24, 'tags:ljaysSKw', `search:${q}`]),
  ]).then(([res, alb]) => {
    if (!alive) return;
    if (res.status === 'rejected' && alb.status === 'rejected') {
      body.replaceChildren(errorBox(res.reason, () => ctx.reload()));
      return;
    }
    const r = res.status === 'fulfilled' ? res.value : {};
    const sections = [];

    const artists = r.contributors_loop || [];
    if (artists.length) {
      const ul = list('Artists');
      for (const a of artists) {
        ul.append(row({
          href: `#/library/artist/${enc(a.contributor_id)}?name=${enc(a.contributor)}`,
          title: a.contributor, iconName: 'person',
          actions: actionButtons(ctx, 'artist_id', a.contributor_id, a.contributor, { compact: true }),
        }));
      }
      sections.push(section('Artists', r.contributors_count ?? artists.length, ul));
    }

    // Prefer the albums query (has artwork); fall back to search's albums_loop.
    const albums = alb.status === 'fulfilled' ? alb.value.albums_loop || [] : [];
    if (albums.length) {
      const grid = albumGrid();
      for (const a of albums) grid.append(albumCard(ctx, a));
      sections.push(section('Albums', alb.value.count ?? albums.length, grid));
    } else if (r.albums_loop?.length) {
      const ul = list('Albums');
      for (const a of r.albums_loop) {
        ul.append(row({
          href: `#/library/album/${enc(a.album_id)}?name=${enc(a.album)}`, title: a.album, iconName: 'album',
          actions: actionButtons(ctx, 'album_id', a.album_id, a.album, { compact: true }),
        }));
      }
      sections.push(section('Albums', r.albums_count ?? r.albums_loop.length, ul));
    }

    const tracks = r.tracks_loop || [];
    if (tracks.length) {
      const ul = list('Tracks');
      ul.classList.add('tracks');
      for (const t of tracks) ul.append(trackRow(ctx, { ...t, id: t.track_id, title: t.track || t.title }, { showAlbum: true }));
      sections.push(section('Tracks', r.tracks_count ?? tracks.length, ul));
    }

    const genres = r.genres_loop || [];
    if (genres.length) {
      const ul = list('Genres');
      for (const g of genres) {
        ul.append(row({
          href: `#/library/genre/${enc(g.genre_id)}?name=${enc(g.genre)}`, title: g.genre, iconName: 'genre',
          actions: actionButtons(ctx, 'genre_id', g.genre_id, g.genre, { compact: true }),
        }));
      }
      sections.push(section('Genres', r.genres_count ?? genres.length, ul));
    }

    body.replaceChildren(...(sections.length ? sections : [emptyBox(`Nothing matches “${q}”.`)]));
  });

  return () => { alive = false; };
}

function section(title, count, content) {
  const more = +count > 0 ? h('span', { class: 'muted' }, plural(count, 'result')) : null;
  return h('section', { class: 'result-section' }, h('h2', {}, title, ' ', more), content);
}
