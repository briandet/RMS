// Server access: the native API (`/api/v1`, see docs/api.md) for player state, commands
// and the pushed event stream; LMS JSON-RPC (`slim.request`) for library browsing and
// settings that the native API does not cover yet.

/**
 * Root URL of the server. The app may be served at "/" or under "/web/", while
 * the JSON-RPC endpoint and artwork always live at the server root.
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

export class RpcError extends Error {
  constructor(message, { network = false } = {}) {
    super(message);
    this.name = 'RpcError';
    this.network = network;
  }
}

let seq = 0;

/**
 * Send one `slim.request`.
 * @param {Array} cmd command array, e.g. ['status', '-', 1, 'tags:al']
 * @param {string} playerId player MAC or '' for server commands
 * @returns {Promise<object>} the `result` object (empty object if none)
 */
export async function rpc(cmd, playerId = '', { timeout = 15000 } = {}) {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), timeout);
  const id = ++seq;
  let res;
  try {
    res = await fetch(serverRoot + 'jsonrpc.js', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id, method: 'slim.request', params: [playerId || '', cmd] }),
      signal: ctrl.signal,
      cache: 'no-store',
    });
  } catch (e) {
    throw new RpcError(e.name === 'AbortError' ? 'Request timed out' : 'Cannot reach server', { network: true });
  } finally {
    clearTimeout(timer);
  }
  if (!res.ok) throw new RpcError(`Server error (HTTP ${res.status})`, { network: res.status >= 500 });
  let data;
  try { data = await res.json(); } catch { throw new RpcError('Invalid response from server'); }
  if (data?.error) throw new RpcError(data.error.message || String(data.error));
  return data?.result || {};
}

// ---------------------------------------------------------------- native API

/** Call `/api/v1/<path>`; resolves to the decoded JSON, throws RpcError. */
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
    throw new RpcError(e.name === 'AbortError' ? 'Request timed out' : 'Cannot reach server', { network: true });
  } finally {
    clearTimeout(timer);
  }
  let data = null;
  try { data = await res.json(); } catch { /* empty or not JSON */ }
  if (!res.ok) throw new RpcError(data?.error || `Server error (HTTP ${res.status})`, { network: res.status >= 500 });
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

/** Native API item for a library key used by the views (`album_id`, `year`, …). */
const ITEM_TYPES = {
  track_id: 'track', album_id: 'album', artist_id: 'artist', genre_id: 'genre',
  playlist_id: 'playlist', folder_id: 'folder',
};
export function itemFor(key, value) {
  if (key === 'year') return { type: 'year', year: +value };
  const type = ITEM_TYPES[key];
  return type ? { type, id: +value } : null;
}

// ---------------------------------------------------------------- artwork
export function coverUrl(coverId, size = 300) {
  if (coverId == null || coverId === '' || coverId === 0 || coverId === '0') return null;
  return `${serverRoot}music/${encodeURIComponent(coverId)}/cover_${size}x${size}_o`;
}

/** Artist picture by `portraitid` (artists/albums/titles tag 4), or null. */
export function portraitUrl(portraitId, size = 300) {
  if (!portraitId) return null;
  return `${serverRoot}contributor/${encodeURIComponent(portraitId)}/image_${size}x${size}_o`;
}

/** artwork_url may be absolute, root-relative, relative (e.g. "imageproxy/..."), or a data URI. */
export function resolveArtwork(url) {
  if (!url) return null;
  url = String(url);
  if (/^(https?:|data:|blob:)/i.test(url)) return url;
  if (url.startsWith('//')) return location.protocol + url;
  return serverRoot + url.replace(/^\/+/, '');
}

/** Best artwork for a track/album-ish loop item. */
export function itemArt(item, size = 300) {
  if (!item) return null;
  if (item.artwork_url) return resolveArtwork(item.artwork_url);
  if (item.image) return resolveArtwork(item.image);
  return coverUrl(item.coverid ?? item.artwork_track_id, size);
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
