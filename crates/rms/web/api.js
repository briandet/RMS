// Server access through the native API (`/api/v1`, see docs/api.md): player state and
// commands, the pushed event stream, library browsing, favorites, Spotify and settings.

/**
 * Root URL of the server. The app may be served at "/" or under "/web/", while
 * the API and artwork always live at the server root.
 * Override with `window.RMS_SERVER_ROOT = 'http://host:9000/'` if needed.
 */
export const serverRoot = (() => {
  if (typeof window !== 'undefined' && window.RMS_SERVER_ROOT) {
    return String(window.RMS_SERVER_ROOT).replace(/\/?$/, '/');
  }
  const p = location.pathname;
  const i = p.indexOf('/web/');
  if (i >= 0) return p.slice(0, i + 1);
  return p.replace(/[^/]*$/, '') || '/';
})();

export class ApiError extends Error {
  constructor(message, { network = false, status = 0 } = {}) {
    super(message);
    this.name = 'ApiError';
    this.network = network;
    this.status = status;
  }
}

// ---------------------------------------------------------------- native API

/** Call `/api/v1/<path>`; resolves to the decoded JSON, throws ApiError. */
export async function api(path, { method = 'GET', body, timeout = 15000 } = {}) {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), timeout);
  let res;
  try {
    res = await fetch(`${serverRoot}api/v1/${path}`, {
      method,
      headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: ctrl.signal,
      cache: 'no-store',
    });
  } catch (e) {
    throw new ApiError(e.name === 'AbortError' ? 'Request timed out' : 'Cannot reach server', { network: true });
  } finally {
    clearTimeout(timer);
  }
  let data = null;
  try { data = await res.json(); } catch { /* empty or not JSON */ }
  if (!res.ok) throw new ApiError(data?.error || `Server error (HTTP ${res.status})`, { network: res.status >= 500, status: res.status });
  return data;
}

const playerPath = (playerId) => `players/${encodeURIComponent(playerId)}`;

/** Send a player command, e.g. `{command: 'volume', value: 30}`. */
export function command(playerId, body) {
  return api(`${playerPath(playerId)}/command`, { method: 'POST', body });
}

export function getPlayer(playerId) {
  return api(playerPath(playerId));
}

export function getQueue(playerId) {
  return api(`${playerPath(playerId)}/queue`);
}

/** What Spotify plays next on a player's Spotify Connect device. @returns {Promise<import('./api-types').SpotifyQueue>} */
export const getSpotifyQueue = (playerId) => api(`${playerPath(playerId)}/spotify-queue`);

// ---------------------------------------------------------------- library & services
//
// Types come from api-types.d.ts (generated from the server's model); lists are `Page`s.

/** @template T @typedef {import('./api-types').Page<T>} Page */
/** @typedef {import('./api-types').Artist} Artist */
/** @typedef {import('./api-types').Album} Album */
/** @typedef {import('./api-types').AlbumDetail} AlbumDetail */
/** @typedef {import('./api-types').Genre} Genre */
/** @typedef {import('./api-types').Year} Year */
/** @typedef {import('./api-types').Track} Track */
/** @typedef {import('./api-types').Playlist} Playlist */
/** @typedef {import('./api-types').FolderEntry} FolderEntry */
/** @typedef {import('./api-types').SearchResults} SearchResults */
/** @typedef {import('./api-types').BrowseItem} BrowseItem */
/** @typedef {import('./api-types').SpotifyStatus} SpotifyStatus */
/** @typedef {import('./api-types').SpotifyLoginResult} SpotifyLoginResult */
/** @typedef {import('./api-types').ServerSettings} ServerSettings */
/** @typedef {import('./api-types').ServerSettingsPatch} ServerSettingsPatch */
/** @typedef {import('./api-types').ScanMode} ScanMode */
/** @typedef {import('./api-types').PlayerSettings} PlayerSettings */
/** @typedef {import('./api-types').PlayerSettingsPatch} PlayerSettingsPatch */

/** `path?a=1&b=2`, leaving out parameters that are null, undefined or ''. */
function withQuery(path, params = {}) {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v != null && v !== '') q.set(k, String(v));
  const s = q.toString();
  return s ? `${path}?${s}` : path;
}

/** @param {{offset?: number, limit?: number, q?: string}} [p] @returns {Promise<Page<Artist>>} */
export const getArtists = (p) => api(withQuery('artists', p));
/** @returns {Promise<Artist>} */
export const getArtist = (id) => api(`artists/${encodeURIComponent(id)}`);
/**
 * @param {{sort?: 'title'|'artist'|'year'|'added', artist?: number, genre?: number, year?: number,
 *   q?: string, offset?: number, limit?: number}} [p]
 * @returns {Promise<Page<Album>>}
 */
export const getAlbums = (p) => api(withQuery('albums', p));
/** @returns {Promise<AlbumDetail>} */
export const getAlbum = (id) => api(`albums/${encodeURIComponent(id)}`);
/** @returns {Promise<Page<Genre>>} */
export const getGenres = (p) => api(withQuery('genres', p));
/** @returns {Promise<Page<Year>>} */
export const getYears = (p) => api(withQuery('years', p));
/** @returns {Promise<Page<Playlist>>} */
export const getPlaylists = (p) => api(withQuery('playlists', p));
/** New playlist: `{name, item?}` (what an Item stands for) or `{name, from_player}` (a queue). @returns {Promise<import('./api-types').Playlist>} */
export const createPlaylist = (body) => api('playlists', { method: 'POST', body });
export const renamePlaylist = (id, name) => api(`playlists/${encodeURIComponent(id)}`, { method: 'PATCH', body: { name } });
export const deletePlaylist = (id) => api(`playlists/${encodeURIComponent(id)}`, { method: 'DELETE' });
/** Keep the playlist's tracks (indexes) in this order; the others are removed. */
export const reorderPlaylist = (id, order) => api(`playlists/${encodeURIComponent(id)}/tracks`, { method: 'PUT', body: { order } });
/** Add what an Item stands for, before track `position` or at the end. @returns {Promise<{count: number}>} */
export const addToPlaylist = (id, item, position) => api(`playlists/${encodeURIComponent(id)}/tracks`, { method: 'POST', body: { item, position } });
/** @returns {Promise<import('./api-types').Track[]>} */
export const getArtistTopTracks = (id) => api(`artists/${encodeURIComponent(id)}/top-tracks`);
/** @returns {Promise<import('./api-types').SimilarArtist[]>} */
export const getSimilarArtists = (id) => api(`artists/${encodeURIComponent(id)}/similar`);
/** @returns {Promise<Playlist>} */
export const getPlaylist = (id) => api(`playlists/${encodeURIComponent(id)}`);
/** @returns {Promise<Page<Track>>} */
export const getPlaylistTracks = (id, p) => api(withQuery(`playlists/${encodeURIComponent(id)}/tracks`, p));
/** Music folder contents; the top of the music folders without `id`. @returns {Promise<Page<FolderEntry>>} */
export const getFolder = (id, p) => api(withQuery('folders', { ...p, id }));
/** @returns {Promise<SearchResults>} */
export const search = (q, limit = 20) => api(withQuery('search', { q, limit }));

/** @returns {Promise<Page<BrowseItem>>} */
export const getFavorites = (id, p) => api(withQuery('favorites', { ...p, id }));
export const addFavorite = (url, title) => api('favorites', { method: 'POST', body: { url, title } });
export const deleteFavorite = (id) => api(`favorites/${encodeURIComponent(id)}`, { method: 'DELETE' });

/** Spotify sign-in state; rejects with status 404 when the server has no Spotify support. @returns {Promise<SpotifyStatus>} */
export const getSpotify = () => api('spotify');
/** @returns {Promise<Page<BrowseItem>>} */
export const getSpotifyItems = (id, p) => api(withQuery('spotify/items', { ...p, id }));
/** Internet radio: the providers, then their menus (`id` from a BrowseItem). @returns {Promise<Page<BrowseItem>>} */
export const getRadio = (id, p) => api(withQuery('radio', { ...p, id }));
/** The Table (docs/api.md "The Table"). */
export const queryTable = (body) => api('query', { method: 'POST', body, timeout: 60000 });
export const getQueryFields = () => api('query/fields');
export const getQueryViews = () => api('query/views');
export const putQueryViews = (views) => api('query/views', { method: 'PUT', body: views });
/** The ListenBrainz menu: recommendations and the playlists made for each account. */
export const getListenBrainz = (id, p) => api(withQuery('listenbrainz', { ...p, id }));
/** Add the ListenBrainz account a user token belongs to. @returns {Promise<import('./api-types').ListenBrainzAccount>} */
export const addListenBrainzAccount = (token) => api('listenbrainz/accounts', { method: 'POST', body: { token } });
export const patchListenBrainzAccount = (user, patch) => api(`listenbrainz/accounts/${encodeURIComponent(user)}`, { method: 'PATCH', body: patch });
export const removeListenBrainzAccount = (user) => api(`listenbrainz/accounts/${encodeURIComponent(user)}`, { method: 'DELETE' });
/** Without `code`: the sign-in page to open (`{url}`); with it: finish (`{user}`). @returns {Promise<SpotifyLoginResult>} */
/** Start a sign-in (`playback`: with Spotify's own client id), or finish one with `code`. */
export const spotifyLogin = (code, playback = false) =>
  api('spotify/login', { method: 'POST', body: code ? { code } : playback ? { playback: true } : {}, timeout: 60000 });
export const spotifyLogout = () => api('spotify/logout', { method: 'POST', body: {} });
/** @param {import('./api-types').SpotifyPatch} patch @returns {Promise<SpotifyStatus>} */
export const patchSpotify = (patch) => api('spotify', { method: 'PATCH', body: patch });
/** Forget the learned Spotify tracks. */
/** @returns {Promise<import('./api-types').LearnedTracks>} */
export const getSpotifyLearned = () => api('spotify/learned');
export const forgetSpotifyLearned = () => api('spotify/learned', { method: 'DELETE' });

/** @returns {Promise<ServerSettings>} */
export const getSettings = () => api('settings');
/** @param {ServerSettingsPatch} patch @returns {Promise<ServerSettings>} */
export const patchSettings = (patch) => api('settings', { method: 'PATCH', body: patch });
/** @param {ScanMode} mode `changes` (rescan), `playlists`, or `full` (clear the library and rescan). */
// ---------------------------------------------------------------- MusicBrainz releases, tags

/** @typedef {import('./api-types').AlbumRelease} AlbumRelease */
/** @typedef {import('./api-types').AlbumTags} AlbumTags */
/** @typedef {import('./api-types').TagEditResult} TagEditResult */
/** @typedef {import('./api-types').TagEdit} TagEdit */

/** @returns {Promise<AlbumRelease>} */
export const getAlbumRelease = (id) => api(`albums/${encodeURIComponent(id)}/release`);
/** Match the album to a release (fetched from MusicBrainz). @returns {Promise<AlbumRelease>} */
export const setAlbumRelease = (id, mbid) => api(`albums/${encodeURIComponent(id)}/release`, { method: 'PUT', body: { mbid }, timeout: 60000 });
/** @returns {Promise<AlbumTags>} */
export const getAlbumTags = (id, release) => api(withQuery(`albums/${encodeURIComponent(id)}/tags`, { release }), { timeout: 60000 });
/** @param {{track_id: number, tags: Record<string, string | null>}[]} changes @returns {Promise<TagEditResult>} */
export const writeTags = (changes) => api('tags', { method: 'POST', body: { changes }, timeout: 300000 });
/** @returns {Promise<import('./api-types').CoverArt>} */
export const getCoverArt = (id, release) => api(withQuery(`albums/${encodeURIComponent(id)}/cover-art`, { release }), { timeout: 60000 });
/** @returns {Promise<TagEditResult>} */
export const setCoverArt = (id, choice, release) => api(withQuery(`albums/${encodeURIComponent(id)}/cover-art`, { release }), { method: 'POST', body: choice, timeout: 300000 });
/** @returns {Promise<import('./api-types').AlbumFiles>} */
export const getAlbumFiles = (id, template) => api(withQuery(`albums/${encodeURIComponent(id)}/files`, { template }), { timeout: 60000 });
/** @returns {Promise<import('./api-types').MoveResult>} */
export const moveAlbumFiles = (id, template) => api(`albums/${encodeURIComponent(id)}/files`, { method: 'POST', body: template ? { template } : {}, timeout: 300000 });
/** @returns {Promise<TagEdit[]>} */
export const getTagEdits = (limit = 10) => api(withQuery('tags/edits', { limit }));
/** @returns {Promise<TagEditResult>} */
export const undoTagEdit = (batch) => api(`tags/edits/${encodeURIComponent(batch)}/undo`, { method: 'POST', body: {}, timeout: 300000 });

export const getTrackAnalysis = (id) => api(`tracks/${encodeURIComponent(id)}/analysis`);

export const getAnalysisProgress = () => api('library/analysis');

export const scanLibrary = (mode) => api('library/scan', { method: 'POST', body: { mode } });

/** @returns {Promise<PlayerSettings>} */
export const getPlayerSettings = (playerId) => api(`${playerPath(playerId)}/settings`);
/** @param {PlayerSettingsPatch} patch @returns {Promise<PlayerSettings>} */
export const patchPlayerSettings = (playerId, patch) => api(`${playerPath(playerId)}/settings`, { method: 'PATCH', body: patch });

/**
 * Subscribe to the event stream.  `handlers` maps event names (`snapshot`, `server`,
 * `player`, `player_removed`) to functions taking the decoded data; `open` / `error` are
 * called when the connection comes up or drops (the browser reconnects by itself, and the
 * server starts every connection with a snapshot).  Returns a function that closes it.
 */
export function connectEvents(handlers) {
  const es = new EventSource(`${serverRoot}api/v1/events`);
  for (const name of ['snapshot', 'server', 'player', 'player_removed']) {
    es.addEventListener(name, (e) => {
      let data;
      try { data = JSON.parse(e.data); } catch { return; }
      handlers[name]?.(data);
    });
  }
  es.addEventListener('open', () => handlers.open?.());
  es.addEventListener('error', () => handlers.error?.(es.readyState === EventSource.CLOSED));
  return () => es.close();
}

/** Native API item types for the keys the views use (`album_id`, …). */
const ITEM_TYPES = {
  track_id: 'track', album_id: 'album', artist_id: 'artist', genre_id: 'genre',
  playlist_id: 'playlist', folder_id: 'folder',
};
/**
 * Native API item for a key used by the views (`album_id`, `year`, `favorite_id`, …).
 * @returns {import('./api-types').Item | null}
 */
export function itemFor(key, value) {
  if (key === 'year') return { type: 'year', year: +value };
  if (key === 'url') return { type: 'url', url: String(value) };
  // favorites and Spotify menu items are addressed by their BrowseItem id string
  if (key === 'favorite_id') return { type: 'favorite', id: String(value) };
  if (key === 'spotify_id') return { type: 'spotify', id: String(value) };
  if (key === 'radio_id') return { type: 'radio', id: String(value) };
  if (key === 'listenbrainz_id') return { type: 'listenbrainz', id: String(value) };
  // what a Table query finds: value is {q, sort}
  if (key === 'query') return { type: 'query', q: String(value?.q ?? ''), sort: value?.sort || [] };
  const type = ITEM_TYPES[key];
  return type ? { type, id: +value } : null;
}

// ---------------------------------------------------------------- artwork
/** artwork may be absolute, root-relative, relative (e.g. "imageproxy/..."), or a data URI. */
export function resolveArtwork(url) {
  if (!url) return null;
  url = String(url);
  if (/^(https?:|data:|blob:)/i.test(url)) return url;
  if (url.startsWith('//')) return location.protocol + url;
  return serverRoot + url.replace(/^\/+/, '');
}

/** Native API artwork path (`music/<id>/cover`, `imageproxy/…/image`) at a given size. */
export function sizedArtwork(path, size = 300) {
  if (!path) return null;
  const sized = /\/(cover|image)$/.test(path) ? `${path}_${size}x${size}_o` : path;
  return resolveArtwork(sized);
}

/** Artwork for whatever the given player is playing right now. */
export function currentArtUrl(playerId, cacheKey = '', size = 600) {
  return `${serverRoot}music/current/cover.jpg?player=${encodeURIComponent(playerId)}&size=${size}&_=${encodeURIComponent(cacheKey)}`;
}
