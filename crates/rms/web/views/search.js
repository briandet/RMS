// Search results: #/search?q=term

import { search, sizedArtwork } from '../api.js';
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

  search(q, 24).then((r) => {
    if (!alive) return;
    const sections = [];

    const artists = r.artists?.items || [];
    if (artists.length) {
      const ul = list('Artists');
      for (const a of artists) {
        ul.append(row({
          href: `#/library/artist/${enc(a.id)}?name=${enc(a.name)}`,
          title: a.name, iconName: 'person', art: a.portrait ? sizedArtwork(a.portrait, 80) : undefined, cls: 'artist',
          actions: actionButtons(ctx, 'artist_id', a.id, a.name, { compact: true }),
        }));
      }
      sections.push(section('Artists', r.artists.total, ul));
    }

    const albums = r.albums?.items || [];
    if (albums.length) {
      const grid = albumGrid();
      for (const a of albums) grid.append(albumCard(ctx, a));
      sections.push(section('Albums', r.albums.total, grid));
    }

    const tracks = r.tracks?.items || [];
    if (tracks.length) {
      const ul = list('Tracks');
      ul.classList.add('tracks');
      for (const t of tracks) ul.append(trackRow(ctx, t, { showAlbum: true }));
      sections.push(section('Tracks', r.tracks.total, ul));
    }

    const genres = r.genres?.items || [];
    if (genres.length) {
      const ul = list('Genres');
      for (const g of genres) {
        ul.append(row({
          href: `#/library/genre/${enc(g.id)}?name=${enc(g.name)}`, title: g.name, iconName: 'genre',
          actions: actionButtons(ctx, 'genre_id', g.id, g.name, { compact: true }),
        }));
      }
      sections.push(section('Genres', r.genres.total, ul));
    }

    body.replaceChildren(...(sections.length ? sections : [emptyBox(`Nothing matches “${q}”.`)]));
  }, (e) => {
    if (alive) body.replaceChildren(errorBox(e, () => ctx.reload()));
  });

  return () => { alive = false; };
}

function section(title, count, content) {
  const more = +count > 0 ? h('span', { class: 'muted' }, plural(count, 'result')) : null;
  return h('section', { class: 'result-section' }, h('h2', {}, title, ' ', more), content);
}
