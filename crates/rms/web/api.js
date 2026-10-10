// LMS JSON-RPC client. Uses only standard Lyrion/Logitech Media Server commands,
// so the UI works against RMS and against a stock LMS.

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

/** `["playlistcontrol","cmd:load","album_id:3"]` helper. action = load|add|insert */
export function playlistControl(playerId, action, key, value) {
  return rpc(['playlistcontrol', `cmd:${action}`, `${key}:${value}`], playerId);
}

// ---------------------------------------------------------------- artwork
export function coverUrl(coverId, size = 300) {
  if (coverId == null || coverId === '' || coverId === 0 || coverId === '0') return null;
  return `${serverRoot}music/${encodeURIComponent(coverId)}/cover_${size}x${size}_o`;
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

/** Artwork for whatever the given player is playing right now. */
export function currentArtUrl(playerId, cacheKey = '', size = 600) {
  return `${serverRoot}music/current/cover.jpg?player=${encodeURIComponent(playerId)}&size=${size}&_=${encodeURIComponent(cacheKey)}`;
}
