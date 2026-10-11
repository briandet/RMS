// Shared application state plus a tiny pub/sub bus.  Player and server objects are the
// native API's (docs/api.md), kept current by the server's event stream.
//
// Events:
//   'players'  players list changed (added, removed, renamed, connected, power)  (store.players)
//   'player'   selected player changed        (store.playerId)
//   'status'   state of the selected player changed                          (currentPlayer())
//   'queue'    queue contents changed         (store.queue)
//   'server'   server info changed            (store.server)
//   'online'   connectivity changed           (store.online)
//   'player-state'    any player's state arrived  (the player)
//   'browser-player'  this browser as a player connected or went away (browser-player.js)

export const store = {
  players: [],
  playerId: null,
  server: null,
  /** performance.now() when each player's state arrived (for time interpolation), by id. */
  receivedAt: new Map(),
  /** Tracks of the selected player's queue. */
  queue: [],
  /** `queue.version` / `queue.length` the queue was fetched for. */
  queueKey: null,
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

/** The selected player's state, or null. */
export function currentPlayer() {
  return store.players.find((p) => p.id === store.playerId) || null;
}

/** Track playing (or about to play) on the selected player, or null. */
export function currentTrack() {
  return currentPlayer()?.now_playing || null;
}

/** Elapsed seconds on the selected player, interpolated locally while playing. */
export function elapsed() {
  const p = currentPlayer();
  if (!p) return 0;
  let t = p.elapsed || 0;
  if (p.mode === 'play' && !p.buffering) t += (performance.now() - (store.receivedAt.get(p.id) || 0)) / 1000;
  const d = p.duration || 0;
  return d > 0 ? Math.min(t, d) : t;
}

/** Apply a local optimistic change to the selected player and notify listeners. */
export function patchPlayer(patch) {
  const p = currentPlayer();
  if (!p) return;
  if ('elapsed' in patch) store.receivedAt.set(p.id, performance.now());
  Object.assign(p, patch);
  emit('status', p);
}
