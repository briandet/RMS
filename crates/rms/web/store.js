// Shared application state plus a tiny pub/sub bus.
//
// Events:
//   'players'  players list changed           (store.players)
//   'player'   selected player changed        (store.playerId)
//   'status'   new status for selected player (store.status)
//   'queue'    queue contents changed         (store.queue)
//   'server'   new serverstatus               (store.server)
//   'online'   connectivity changed           (store.online)

export const store = {
  players: [],
  playerId: null,
  server: null,
  status: null,
  statusAt: 0, // performance.now() when status was received (for time interpolation)
  queue: [],
  online: true,
};

const listeners = new Map();

export function on(event, fn) {
  if (!listeners.has(event)) listeners.set(event, new Set());
  listeners.get(event).add(fn);
  return () => listeners.get(event)?.delete(fn);
}

export function emit(event, payload) {
  for (const fn of [...(listeners.get(event) || [])]) {
    try { fn(payload); } catch (e) { console.error(`listener for '${event}' failed`, e); }
  }
}

export function currentPlayer() {
  return store.players.find((p) => p.playerid === store.playerId) || null;
}

/** Current track item (from `status - 1`), or null. */
export function currentTrack() {
  return store.status?.playlist_loop?.[0] || null;
}

/** Elapsed seconds, interpolated locally while playing. */
export function elapsed() {
  const s = store.status;
  if (!s) return 0;
  let t = +s.time || 0;
  if (s.mode === 'play') t += (performance.now() - store.statusAt) / 1000;
  const d = +s.duration || 0;
  return d > 0 ? Math.min(t, d) : t;
}

/** Apply a local optimistic change to the status and notify listeners. */
export function patchStatus(patch) {
  if (!store.status) return;
  if ('time' in patch) store.statusAt = performance.now();
  Object.assign(store.status, patch);
  emit('status', store.status);
}
