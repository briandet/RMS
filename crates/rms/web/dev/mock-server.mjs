#!/usr/bin/env node
// Development-only mock of the RMS native API and LMS JSON-RPC. NOT loaded by the web app.
//
//   node crates/rms/web/dev/mock-server.mjs            # http://localhost:9000/
//   PORT=9001 PLAYERS=0 node .../mock-server.mjs        # no players (empty state)
//
// Serves the web directory at / and /web/, answers /api/v1/* (including the event stream)
// and POST /jsonrpc.js with fake but plausible data, simulates playback progress, and
// renders SVG cover art for /music/*.

import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const WEB_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const PORT = +(process.env.PORT || 9000);
const NUM_PLAYERS = process.env.PLAYERS === undefined ? 2 : +process.env.PLAYERS;
const VERBOSE = !!process.env.VERBOSE;

// ------------------------------------------------------------------ fake library
let rnd = 42;
const rand = () => ((rnd = (rnd * 1103515245 + 12345) % 2147483648) / 2147483648);
const pick = (a) => a[Math.floor(rand() * a.length)];

const FIRST = ['Amber', 'Blue', 'Crimson', 'Silver', 'Northern', 'Velvet', 'Electric', 'Quiet', 'Golden', 'Midnight', 'Paper', 'Glass', 'Hollow', 'Neon', 'Wild'];
const SECOND = ['Foxes', 'Harbor', 'Lanterns', 'Orchard', 'Machines', 'Sparrows', 'Tides', 'Pilots', 'Atlas', 'Echoes', 'Rivers', 'Satellites', 'Wolves', 'Gardens'];
const WORDS = ['Light', 'Run', 'Summer', 'Ghost', 'Home', 'Fire', 'Stay', 'Ocean', 'City', 'Dream', 'Static', 'Gravity', 'Signal', 'Winter', 'Heart', 'Road', 'Morning', 'Shadows', 'Wave', 'Paris'];
const GENRES = ['Rock', 'Jazz', 'Electronic', 'Folk', 'Classical', 'Hip-Hop', 'Ambient', 'Pop'];

const genres = GENRES.map((g, i) => ({ id: i + 1, genre: g }));
const artists = [];
const albums = [];
const tracks = [];
const seen = new Set();
while (artists.length < 45) {
  const name = `${pick(FIRST)} ${pick(SECOND)}`;
  if (seen.has(name)) continue;
  seen.add(name);
  const id = artists.length + 1;
  // every third artist has a picture (`portraitid`, tag 4)
  artists.push({ id, artist: name, ...(id % 3 === 0 ? { portraitid: `p${id}` } : {}) });
}
artists.sort((a, b) => a.artist.localeCompare(b.artist));
for (const ar of artists) {
  const n = 1 + Math.floor(rand() * 5);
  for (let k = 0; k < n; k++) {
    const id = albums.length + 1;
    const title = `${pick(WORDS)} ${pick(['of', 'in', 'and', 'for'])} ${pick(WORDS)}`;
    const year = 1968 + Math.floor(rand() * 57);
    const genre = pick(genres);
    const album = { id, album: title, artist: ar.artist, artist_id: ar.id, year, genre_id: genre.id, added: rand(), tracks: [] };
    const nt = 6 + Math.floor(rand() * 9);
    for (let t = 1; t <= nt; t++) {
      const tid = tracks.length + 1;
      const track = {
        id: tid, title: `${pick(WORDS)}${rand() < .4 ? ' ' + pick(WORDS) : ''}`, artist: ar.artist, artist_id: ar.id,
        album: title, album_id: id, duration: 120 + Math.floor(rand() * 300), tracknum: t, year, genre: genre.genre,
        coverid: `c${id}`, url: `file:///music/${encodeURIComponent(ar.artist)}/${encodeURIComponent(title)}/${t}.flac`, type: 'flc',
      };
      tracks.push(track);
      album.tracks.push(track);
    }
    album.artwork_track_id = `c${id}`;
    albums.push(album);
  }
}
albums.sort((a, b) => a.album.localeCompare(b.album));
const albumById = new Map(albums.map((a) => [a.id, a]));
const trackById = new Map(tracks.map((t) => [t.id, t]));

const playlists = [
  { id: 9001, playlist: 'Morning Coffee', trackIds: tracks.slice(3, 21).map((t) => t.id) },
  { id: 9002, playlist: 'Road Trip', trackIds: tracks.filter((_, i) => i % 7 === 0).slice(0, 40).map((t) => t.id) },
  { id: 9003, playlist: 'Empty List', trackIds: [] },
];

const RADIO = { url: 'http://radio.example.com/stream.mp3', name: 'Example FM' };
const favorites = [
  { id: 'f0', name: RADIO.name, type: 'audio', url: RADIO.url, isaudio: 1, hasitems: 0, image: 'html/images/radio.png' },
  { id: 'f1', name: 'Chill', type: 'text', isaudio: 0, hasitems: 1, items: [
    { id: 'f1.0', name: 'Ambient Radio', type: 'audio', url: 'http://radio.example.com/ambient', isaudio: 1, hasitems: 0 },
    { id: 'f1.1', name: tracks[5].title + ' – ' + tracks[5].artist, type: 'audio', url: tracks[5].url, isaudio: 1, hasitems: 0 },
  ] },
];

// Folder tree: root -> artist folders -> album folders -> tracks
const folders = new Map(); // id -> {id, filename, type, children:[...]}
let folderSeq = 50000;
const rootFolder = { id: 0, children: [] };
for (const ar of artists.slice(0, 20)) {
  const f = { id: ++folderSeq, filename: ar.artist, type: 'folder', children: [] };
  for (const al of albums.filter((a) => a.artist_id === ar.id)) {
    const af = { id: ++folderSeq, filename: al.album, type: 'folder', children: al.tracks.map((t) => ({ id: t.id, filename: `${String(t.tracknum).padStart(2, '0')} ${t.title}.flac`, type: 'track', title: t.title, duration: t.duration, url: t.url })) };
    af.children.push({ id: ++folderSeq, filename: 'cover.txt', type: 'unknown' });
    folders.set(af.id, af);
    f.children.push(af);
  }
  folders.set(f.id, f);
  rootFolder.children.push(f);
}
rootFolder.children.push({ id: 9001, filename: 'mix.m3u', type: 'playlist' });
folders.set(0, rootFolder);

// ------------------------------------------------------------------ server state
const prefs = { libraryname: 'Mock Library', mediadirs: ['/srv/music', '/mnt/nas/flac'] };
const scan = { running: false, start: 0, total: tracks.length };
let lastscan = Math.floor(Date.now() / 1000) - 3600 * 5;

function makePlayer(i) {
  const mac = `aa:bb:cc:00:00:0${i + 1}`;
  return {
    playerid: mac, name: ['Living Room', 'Kitchen', 'Office'][i] || `Player ${i + 1}`,
    model: ['squeezelite', 'wiim', 'baby'][i] || 'squeezelite', modelname: ['SqueezeLite', 'WiiM Pro', 'Squeezebox Radio'][i] || 'SqueezeLite',
    ip: `192.168.1.${20 + i}:${40000 + i}`, connected: 1, power: 1,
    mode: 'stop', queue: [], index: 0, elapsedBase: 0, startedAt: 0,
    volume: 40, muted: false, shuffle: 0, repeat: 0, timestamp: Date.now() / 1000,
    prefs: { transitionType: 0, transitionDuration: 5, replayGainMode: 0, maxBitrate: 0, digitalVolumeControl: 1 },
    syncGroup: null,
  };
}
const players = Array.from({ length: NUM_PLAYERS }, (_, i) => makePlayer(i));
if (players[0]) {
  players[0].queue = albums[3].tracks.map((t) => ({ ...t }));
  players[0].index = 2;
  play(players[0], 37);
}

function elapsedOf(p) {
  if (p.mode === 'play') return p.elapsedBase + (Date.now() - p.startedAt) / 1000;
  return p.elapsedBase;
}
function play(p, at = 0) { p.mode = 'play'; p.elapsedBase = at; p.startedAt = Date.now(); p.power = 1; }
function setIndex(p, i, keepMode = false) {
  if (!p.queue.length) { p.mode = 'stop'; return; }
  p.index = ((i % p.queue.length) + p.queue.length) % p.queue.length;
  if (keepMode && p.mode !== 'play') { p.elapsedBase = 0; return; }
  play(p, 0);
}
function touch(p) { p.timestamp = Date.now() / 1000; }

// Advance tracks that have finished.
function advance(p) {
  const cur = p.queue[p.index];
  if (p.mode !== 'play' || !cur || !cur.duration) return;
  let e = elapsedOf(p);
  while (e >= cur.duration) {
    e -= cur.duration;
    if (p.repeat === 1) { play(p, e); return; }
    if (p.index + 1 >= p.queue.length) {
      if (p.repeat === 2) { p.index = 0; play(p, e); return; }
      p.mode = 'stop'; p.elapsedBase = 0; return;
    }
    p.index++;
    play(p, e);
    return;
  }
}

// ------------------------------------------------------------------ helpers
const paged = (list, start, count) => list.slice(+start || 0, (+start || 0) + (+count || 0));
function params(args) {
  const out = {};
  for (const a of args) if (typeof a === 'string' && a.includes(':')) { const i = a.indexOf(':'); out[a.slice(0, i)] = a.slice(i + 1); }
  return out;
}
const albumOut = (a) => ({ id: a.id, album: a.album, artist: a.artist, artist_id: a.artist_id, year: a.year, artwork_track_id: a.artwork_track_id, textkey: a.album[0].toUpperCase() });
const trackOut = (t, extra = {}) => ({ id: t.id, title: t.title, artist: t.artist, artist_id: t.artist_id, album: t.album, album_id: t.album_id, duration: t.duration, tracknum: t.tracknum, coverid: t.coverid, url: t.url, ...extra });

function tracksFor(p) {
  if (p.album_id) return albumById.get(+p.album_id)?.tracks || [];
  if (p.artist_id) return albums.filter((a) => a.artist_id === +p.artist_id).flatMap((a) => a.tracks);
  if (p.genre_id) return albums.filter((a) => a.genre_id === +p.genre_id).flatMap((a) => a.tracks);
  if (p.year) return albums.filter((a) => a.year === +p.year).flatMap((a) => a.tracks);
  if (p.track_id) return [trackById.get(+p.track_id)].filter(Boolean);
  if (p.playlist_id) return (playlists.find((x) => x.id === +p.playlist_id)?.trackIds || []).map((id) => trackById.get(id));
  if (p.folder_id) {
    const out = [];
    const walk = (f) => { for (const c of f.children || []) { if (c.type === 'track') out.push(trackById.get(c.id)); else if (c.type === 'folder') walk(c); } };
    const f = folders.get(+p.folder_id);
    if (f) walk(f);
    return out.filter(Boolean);
  }
  return [];
}

function radioTrack(url, name) {
  return { id: -Date.now(), title: name || 'Stream', url, remote: 1, duration: 0, artwork_url: 'imageproxy/radio.png', remote_title: name };
}

function loadInto(p, list, action, start = 0) {
  const items = list.map((t) => ({ ...t }));
  if (action === 'load') { p.queue = items; p.index = Math.min(+start || 0, Math.max(0, items.length - 1)); if (items.length) play(p, 0); else p.mode = 'stop'; }
  else if (action === 'add') { p.queue.push(...items); if (p.mode === 'stop' && p.queue.length === items.length) { p.index = 0; } }
  else if (action === 'insert') p.queue.splice(p.index + 1, 0, ...items);
  touch(p);
}

// ------------------------------------------------------------------ commands
function handle(pid, cmd) {
  const p = players.find((x) => x.playerid === pid);
  const [c0, c1, c2] = cmd;
  const kv = params(cmd);
  if (p) advance(p);

  switch (c0) {
    case 'serverstatus': {
      if (scan.running && Date.now() - scan.start > 12000) { scan.running = false; lastscan = Math.floor(Date.now() / 1000); }
      const r = {
        version: '9.0.0-mock', uuid: 'mock', 'player count': players.length, lastscan: String(lastscan),
        'info total albums': albums.length, 'info total artists': artists.length, 'info total songs': tracks.length, 'info total genres': genres.length,
        players_loop: players.map((x) => ({ playerid: x.playerid, name: x.name, model: x.model, modelname: x.modelname, connected: x.connected, power: x.power, isplaying: x.mode === 'play' ? 1 : 0, ip: x.ip, canpoweroff: 1, isplayer: 1 })),
      };
      if (scan.running) {
        const frac = Math.min(1, (Date.now() - scan.start) / 12000);
        Object.assign(r, { rescan: 1, progressname: frac < .7 ? 'directory' : 'discovering_files', progressdone: Math.floor(frac * scan.total), progresstotal: scan.total });
      }
      return r;
    }
    case 'status': {
      if (!p) throw new Error('unknown player');
      const cur = p.queue[p.index];
      const r = {
        player_name: p.name, player_connected: p.connected, power: p.power, mode: p.mode,
        time: cur ? Math.round(elapsedOf(p) * 1000) / 1000 : 0, duration: cur?.duration || 0, can_seek: cur && !cur.remote ? 1 : 0,
        rate: 1, 'mixer volume': p.muted ? -p.volume : p.volume, 'playlist repeat': p.repeat, 'playlist shuffle': p.shuffle,
        playlist_cur_index: p.queue.length ? p.index : undefined, playlist_timestamp: p.timestamp, playlist_tracks: p.queue.length,
        digital_volume_control: p.prefs.digitalVolumeControl, seq_no: 0,
      };
      if (cur?.remote) {
        r.remote = 1;
        const songs = ['Northern Lights', 'Signal Fire', 'Paper Moon'];
        const song = songs[Math.floor(Date.now() / 30000) % songs.length];
        r.current_title = `DJ Mock - ${song}`;
        r.remoteMeta = { id: cur.id, title: song, artist: 'DJ Mock', album: '', remote_title: cur.remote_title, artwork_url: cur.artwork_url, remote: 1, url: cur.url };
      }
      if (p.syncGroup) {
        const g = players.filter((x) => x.syncGroup === p.syncGroup);
        r.sync_master = g[0].playerid;
        r.sync_slaves = g.slice(1).map((x) => x.playerid).join(',');
      }
      const [start, count] = [c1, c2];
      let items = [];
      if (start === '-') items = cur ? [{ ...cur, 'playlist index': p.index }] : [];
      else items = paged(p.queue.map((t, i) => ({ ...t, 'playlist index': i })), start, count);
      if (items.length) r.playlist_loop = items.map((t) => (t.remote ? { 'playlist index': t['playlist index'], id: t.id, title: t.title, url: t.url, remote: 1, remote_title: t.remote_title, artwork_url: t.artwork_url } : { ...trackOut(t), 'playlist index': t['playlist index'] }));
      return r;
    }
    case 'play': if (p && p.queue.length) { if (p.mode !== 'play') play(p, p.mode === 'pause' ? p.elapsedBase : 0); } return {};
    case 'stop': if (p) { p.mode = 'stop'; p.elapsedBase = 0; } return {};
    case 'pause': {
      if (!p) return {};
      const want = c1 === undefined ? (p.mode === 'play' ? 1 : 0) : +c1;
      if (want === 1 && p.mode === 'play') { p.elapsedBase = elapsedOf(p); p.mode = 'pause'; }
      else if (want === 0 && p.mode === 'pause') play(p, p.elapsedBase);
      return {};
    }
    case 'time': if (p) { const t = Math.max(0, +c1 || 0); if (p.mode === 'play') play(p, t); else p.elapsedBase = t; } return {};
    case 'power': if (p) { p.power = +c1 ? 1 : 0; if (!p.power && p.mode === 'play') { p.elapsedBase = elapsedOf(p); p.mode = 'pause'; } } return {};
    case 'name': if (p && c1 && c1 !== '?') p.name = String(c1); return { _value: p?.name };
    case 'mixer': {
      if (!p) return {};
      if (c1 === 'volume') {
        if (c2 === '?') return { _volume: p.volume };
        const v = String(c2);
        p.volume = Math.max(0, Math.min(100, /^[+-]/.test(v) ? p.volume + +v : +v));
        p.muted = false;
      } else if (c1 === 'muting') p.muted = c2 === 'toggle' || c2 === undefined ? !p.muted : !!+c2;
      return {};
    }
    case 'sync': {
      if (!p) return {};
      if (c1 === '-') { p.syncGroup = null; const g = players.filter((x) => x.syncGroup); for (const x of g) if (players.filter((y) => y.syncGroup === x.syncGroup).length < 2) x.syncGroup = null; }
      else { const o = players.find((x) => x.playerid === c1); if (o) { const g = p.syncGroup || o.syncGroup || `g${Date.now()}`; p.syncGroup = g; o.syncGroup = g; } }
      return {};
    }
    case 'playerpref': {
      if (!p) return {};
      if (c2 === '?') return { _p2: p.prefs[c1] };
      p.prefs[c1] = isNaN(+c2) ? c2 : +c2;
      return {};
    }
    case 'pref': {
      if (c2 === '?') return { _p2: prefs[c1] };
      prefs[c1] = c2;
      return {};
    }
    case 'playlist': {
      if (!p) return {};
      switch (c1) {
        case 'index': {
          const v = String(c2);
          if (/^[+-]/.test(v)) setIndex(p, p.index + +v);
          else setIndex(p, +v);
          return {};
        }
        case 'delete': {
          const i = +c2;
          if (i >= 0 && i < p.queue.length) {
            p.queue.splice(i, 1);
            if (i < p.index) p.index--;
            else if (i === p.index) { if (p.index >= p.queue.length) { p.index = 0; p.mode = 'stop'; } else if (p.mode === 'play') play(p, 0); }
            if (!p.queue.length) p.mode = 'stop';
            touch(p);
          }
          return {};
        }
        case 'move': {
          const from = +c2, to = +cmd[3];
          if (from >= 0 && from < p.queue.length && to >= 0 && to < p.queue.length && from !== to) {
            const curTrack = p.queue[p.index];
            const [t] = p.queue.splice(from, 1);
            p.queue.splice(to, 0, t);
            p.index = p.queue.indexOf(curTrack);
            touch(p);
          }
          return {};
        }
        case 'clear': p.queue = []; p.index = 0; p.mode = 'stop'; p.elapsedBase = 0; touch(p); return {};
        case 'shuffle': p.shuffle = +c2 || 0; touch(p); return {};
        case 'repeat': p.repeat = +c2 || 0; return {};
        case 'play': case 'add': case 'insert': {
          const t = tracks.find((x) => x.url === c2);
          const item = t || radioTrack(c2, cmd[3] || 'Stream');
          loadInto(p, [item], c1 === 'play' ? 'load' : c1);
          return {};
        }
        default: return {};
      }
    }
    case 'playlistcontrol': {
      if (!p) return {};
      const list = tracksFor(kv);
      loadInto(p, list, kv.cmd || 'load', kv.play_index);
      return { count: list.length };
    }
    case 'artists': {
      let list = artists;
      if (kv.artist_id) list = list.filter((a) => a.id === +kv.artist_id);
      if (kv.search) list = list.filter((a) => a.artist.toLowerCase().includes(kv.search.toLowerCase()));
      return { count: list.length, artists_loop: paged(list, c1, c2).map((a) => ({ ...a, textkey: a.artist[0].toUpperCase() })) };
    }
    case 'albums': {
      let list = albums;
      if (kv.album_id) list = list.filter((a) => a.id === +kv.album_id);
      if (kv.artist_id) list = list.filter((a) => a.artist_id === +kv.artist_id);
      if (kv.genre_id) list = list.filter((a) => a.genre_id === +kv.genre_id);
      if (kv.year) list = list.filter((a) => a.year === +kv.year);
      if (kv.search) list = list.filter((a) => a.album.toLowerCase().includes(kv.search.toLowerCase()));
      if (kv.sort === 'new') list = [...list].sort((a, b) => b.added - a.added);
      if (kv.sort === 'artflow') list = [...list].sort((a, b) => a.artist.localeCompare(b.artist) || a.year - b.year);
      return { count: list.length, albums_loop: paged(list, c1, c2).map(albumOut) };
    }
    case 'titles': case 'tracks': {
      let list = tracksFor(kv);
      if (!Object.keys(kv).some((k) => ['album_id', 'artist_id', 'genre_id', 'year', 'track_id'].includes(k))) list = tracks;
      if (kv.search) list = list.filter((t) => t.title.toLowerCase().includes(kv.search.toLowerCase()));
      if (kv.sort === 'tracknum') list = [...list].sort((a, b) => a.tracknum - b.tracknum);
      return { count: list.length, titles_loop: paged(list, c1, c2).map((t) => trackOut(t)) };
    }
    case 'genres': return { count: genres.length, genres_loop: paged(genres, c1, c2) };
    case 'years': {
      const ys = [...new Set(albums.map((a) => a.year))].sort((a, b) => b - a).map((year) => ({ year }));
      return { count: ys.length, years_loop: paged(ys, c1, c2) };
    }
    case 'playlists': {
      if (c1 === 'tracks') {
        const pl = playlists.find((x) => x.id === +kv.playlist_id);
        const list = (pl?.trackIds || []).map((id) => trackById.get(id));
        return { count: list.length, playlisttracks_loop: paged(list, c2, cmd[3]).map((t, i) => trackOut(t, { 'playlist index': i })) };
      }
      return { count: playlists.length, playlists_loop: paged(playlists, c1, c2).map(({ id, playlist }) => ({ id, playlist, url: `file:///playlists/${encodeURIComponent(playlist)}.m3u` })) };
    }
    case 'search': {
      const term = (kv.term || '').toLowerCase();
      const ca = artists.filter((a) => a.artist.toLowerCase().includes(term));
      const al = albums.filter((a) => a.album.toLowerCase().includes(term));
      const tr = tracks.filter((t) => t.title.toLowerCase().includes(term));
      const ge = genres.filter((g) => g.genre.toLowerCase().includes(term));
      const r = { count: ca.length + al.length + tr.length + ge.length };
      if (ca.length) { r.contributors_count = ca.length; r.contributors_loop = paged(ca, c1, c2).map((a) => ({ contributor_id: a.id, contributor: a.artist })); }
      if (al.length) { r.albums_count = al.length; r.albums_loop = paged(al, c1, c2).map((a) => ({ album_id: a.id, album: a.album })); }
      if (tr.length) { r.tracks_count = tr.length; r.tracks_loop = paged(tr, c1, c2).map((t) => ({ track_id: t.id, track: t.title })); }
      if (ge.length) { r.genres_count = ge.length; r.genres_loop = paged(ge, c1, c2).map((g) => ({ genre_id: g.id, genre: g.genre })); }
      return r;
    }
    case 'musicfolder': {
      const f = folders.get(+(kv.folder_id || 0));
      if (!f) throw new Error('no such folder');
      const list = f.children.map(({ id, filename, type, title, duration, url }) => ({ id, filename, type, title, duration, url }));
      return { count: list.length, folder_loop: paged(list, c1, c2) };
    }
    case 'favorites': {
      if (c1 === 'items') {
        let list = favorites;
        if (kv.item_id) list = favorites.find((f) => f.id === kv.item_id)?.items || [];
        return { count: list.length, title: 'Favorites', loop_loop: paged(list, c2, cmd[3]).map(({ items, ...f }) => f) };
      }
      if (c1 === 'playlist' && p) {
        const all = [...favorites, ...favorites.flatMap((f) => f.items || [])];
        const f = all.find((x) => x.id === kv.item_id);
        if (!f) throw new Error('no such favorite');
        const t = tracks.find((x) => x.url === f.url) || radioTrack(f.url, f.name);
        loadInto(p, [t], c2 === 'play' ? 'load' : c2);
        return {};
      }
      if (c1 === 'add') {
        favorites.push({ id: `f${favorites.length}`, name: kv.title || kv.url, type: 'audio', url: kv.url, isaudio: 1, hasitems: 0 });
        return { count: 1 };
      }
      return {};
    }
    case 'rescan': case 'wipecache':
      scan.running = true; scan.start = Date.now();
      return {};
    default:
      throw new Error(`mock: unsupported command ${JSON.stringify(cmd)}`);
  }
}

// ------------------------------------------------------------------ native API (/api/v1)
function serverOut() {
  if (scan.running && Date.now() - scan.start > 12000) { scan.running = false; lastscan = Math.floor(Date.now() / 1000); }
  const r = {
    name: prefs.libraryname, version: '0.1.0-mock', uuid: 'mock', last_scan: lastscan,
    library: { albums: albums.length, artists: artists.length, genres: genres.length, tracks: tracks.length, duration: tracks.reduce((s, t) => s + t.duration, 0) },
  };
  if (scan.running) {
    const frac = Math.min(1, (Date.now() - scan.start) / 12000);
    r.scan = { step: frac < .7 ? 'directory' : 'discovering_files', done: Math.floor(frac * scan.total), total: scan.total };
  }
  return r;
}

function trackNative(t) {
  if (t.remote) {
    const songs = ['Northern Lights', 'Signal Fire', 'Paper Moon'];
    const song = songs[Math.floor(Date.now() / 30000) % songs.length];
    return { url: t.url, title: song, artist: 'DJ Mock', artwork: 'imageproxy/radio/image', remote: true, station: t.remote_title };
  }
  return {
    id: t.id, url: t.url, title: t.title, artist: t.artist, artist_id: t.artist_id, album: t.album, album_id: t.album_id,
    duration: t.duration, track_number: t.tracknum, year: t.year, artwork: `music/${t.coverid}/cover`, remote: false, format: t.type,
  };
}

function playerOut(p) {
  advance(p);
  const cur = p.queue[p.index];
  return {
    id: p.playerid, name: p.name, model: p.model, model_name: p.modelname, ip: p.ip.split(':')[0], firmware: 'mock',
    connected: !!p.connected, power: !!p.power, mode: p.mode, buffering: false,
    elapsed: cur ? Math.round(elapsedOf(p) * 1000) / 1000 : 0,
    duration: cur?.duration || undefined, can_seek: !!cur && !cur.remote,
    volume: p.volume, muted: p.muted, volume_fixed: !p.prefs.digitalVolumeControl,
    shuffle: ['off', 'songs', 'albums'][p.shuffle], repeat: ['off', 'one', 'all'][p.repeat],
    queue: { version: p.timestamp, length: p.queue.length, index: p.queue.length ? p.index : undefined },
    now_playing: cur ? trackNative(cur) : undefined,
    synced_with: p.syncGroup ? players.filter((x) => x !== p && x.syncGroup === p.syncGroup).map((x) => x.playerid) : [],
  };
}

const ITEM_KEYS = { track: 'track_id', album: 'album_id', artist: 'artist_id', genre: 'genre_id', playlist: 'playlist_id', folder: 'folder_id' };
const itemParam = (it) => (it.type === 'year' ? `year:${it.year}` : `${ITEM_KEYS[it.type]}:${it.id}`);

/** Native command → the equivalent LMS command(s) on the mock. */
function nativeCommand(p, b) {
  const id = p.playerid;
  const lms = (...cmd) => handle(id, cmd.map(String));
  const inQueue = (i) => { if (!(i >= 0 && i < p.queue.length)) throw Object.assign(new Error(`index ${i} is out of range`), { status: 400 }); };
  switch (b.command) {
    case 'play': p.power = 1; return lms(p.mode === 'pause' ? 'pause' : 'play', ...(p.mode === 'pause' ? [0] : []));
    case 'pause': return lms('pause', 1);
    case 'toggle_pause': return lms('pause');
    case 'stop': return lms('stop');
    case 'next': return lms('playlist', 'index', '+1');
    case 'previous': return lms('playlist', 'index', '-1');
    case 'seek': return lms('time', b.position);
    case 'volume': return lms('mixer', 'volume', b.value);
    case 'mute': return lms('mixer', 'muting', b.muted == null ? 'toggle' : b.muted ? 1 : 0);
    case 'power': return lms('power', b.on ? 1 : 0);
    case 'shuffle': return lms('playlist', 'shuffle', ['off', 'songs', 'albums'].indexOf(b.mode));
    case 'repeat': return lms('playlist', 'repeat', ['off', 'one', 'all'].indexOf(b.mode));
    case 'jump': inQueue(b.index); return lms('playlist', 'index', b.index);
    case 'move': inQueue(b.from); inQueue(b.to); return lms('playlist', 'move', b.from, b.to);
    case 'remove': inQueue(b.index); return lms('playlist', 'delete', b.index);
    case 'clear': return lms('playlist', 'clear');
    case 'enqueue': {
      const mode = b.mode || 'play';
      if (mode === 'play') p.power = 1;
      if (b.item.type === 'url') return lms('playlist', { play: 'play', add: 'add', next: 'insert' }[mode], b.item.url);
      const action = { play: 'load', add: 'add', next: 'insert' }[mode];
      return lms('playlistcontrol', `cmd:${action}`, itemParam(b.item), ...(b.start != null ? [`play_index:${b.start}`] : []));
    }
    case 'mix': {
      const seed = tracksFor({ [ITEM_KEYS[b.seed.type] || 'year']: b.seed.id ?? b.seed.year });
      const genre = seed[0]?.genre;
      const list = [...seed.slice(0, 1), ...tracks.filter((t) => t.genre === genre && t !== seed[0]).slice(0, 19)];
      p.power = 1;
      loadInto(p, list, 'load');
      return { count: list.length };
    }
    case 'sync': return lms('sync', b.with);
    case 'unsync': return lms('sync', '-');
    case 'rename': return lms('name', b.name);
    default: throw Object.assign(new Error(`unknown command ${b.command}`), { status: 400 });
  }
}

// Event stream: push what changed, like the real server (position only when it jumps).
const sseClients = new Set();
const sent = new Map(); // player id -> {json without elapsed, elapsed, at, mode}
let sentServer = '';

function sse(res, name, data) { res.write(`event: ${name}\ndata: ${JSON.stringify(data)}\n\n`); }
function broadcast(name, data) { for (const res of sseClients) sse(res, name, data); }

function publishChanges() {
  if (!sseClients.size) return;
  const srv = serverOut();
  const sj = JSON.stringify(srv);
  if (sj !== sentServer) { sentServer = sj; broadcast('server', { type: 'server', server: srv }); }
  const now = Date.now();
  for (const p of players) {
    const out = playerOut(p);
    const { elapsed, ...rest } = out;
    const key = JSON.stringify(rest);
    const old = sent.get(out.id);
    const expected = old ? old.elapsed + (old.mode === 'play' ? (now - old.at) / 1000 : 0) : 0;
    if (!old || old.key !== key || Math.abs(elapsed - expected) > 1) {
      sent.set(out.id, { key, elapsed, at: now, mode: out.mode });
      broadcast('player', { type: 'player', player: out });
    }
  }
}
setInterval(publishChanges, 1000);

function serveEvents(req, res) {
  res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache', Connection: 'keep-alive' });
  sse(res, 'snapshot', { type: 'snapshot', server: serverOut(), players: players.map(playerOut) });
  sseClients.add(res);
  req.on('close', () => sseClients.delete(res));
}

function serveApi(req, res, url) {
  const json = (status, data) => { res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(data)); };
  const parts = url.pathname.split('/').slice(3).map(decodeURIComponent); // after /api/v1/
  if (parts[0] === 'events') return serveEvents(req, res);
  if (parts[0] === 'server') return json(200, serverOut());
  if (parts[0] !== 'players') return json(404, { error: 'no such resource' });
  if (!parts[1]) return json(200, players.map(playerOut));
  const p = players.find((x) => x.playerid === parts[1]);
  if (!p) return json(404, { error: `unknown player ${parts[1]}` });
  if (!parts[2]) return json(200, playerOut(p));
  if (parts[2] === 'queue') {
    const offset = +url.searchParams.get('offset') || 0;
    const limit = url.searchParams.has('limit') ? +url.searchParams.get('limit') : Infinity;
    return json(200, { version: p.timestamp, length: p.queue.length, index: p.queue.length ? p.index : undefined, offset, items: p.queue.slice(offset, offset + limit).map(trackNative) });
  }
  if (parts[2] === 'command' && req.method === 'POST') {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => {
      try {
        const r = nativeCommand(p, JSON.parse(body));
        if (VERBOSE) console.log(p.playerid, body);
        json(200, r?.count != null ? { count: r.count } : {});
        setTimeout(publishChanges, 20);
      } catch (e) {
        json(e.status || 422, { error: e.message });
      }
    });
    return;
  }
  json(404, { error: 'no such resource' });
}

// ------------------------------------------------------------------ http
const MIME = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.svg': 'image/svg+xml', '.png': 'image/png', '.json': 'application/json' };

function coverSvg(seed, label) {
  let hsh = 0;
  for (const ch of String(seed)) hsh = (hsh * 31 + ch.charCodeAt(0)) >>> 0;
  const h1 = hsh % 360, h2 = (h1 + 40 + (hsh >> 8) % 120) % 360;
  const esc = (s) => String(s).replace(/[<&>"]/g, (c) => `&#${c.charCodeAt(0)};`);
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 300 300"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="hsl(${h1},65%,55%)"/><stop offset="1" stop-color="hsl(${h2},60%,30%)"/></linearGradient></defs><rect width="300" height="300" fill="url(#g)"/><circle cx="${80 + hsh % 140}" cy="${80 + (hsh >> 4) % 140}" r="${40 + hsh % 60}" fill="rgba(255,255,255,.15)"/><text x="20" y="270" font-family="sans-serif" font-size="22" font-weight="700" fill="#fff">${esc(label).slice(0, 22)}</text></svg>`;
}

function serveMusic(url, res) {
  // /music/<coverid>/cover_WxH_o   or   /music/current/cover.jpg?player=...
  const parts = url.pathname.split('/');
  let id = decodeURIComponent(parts[2] || '');
  let label = 'RMS';
  if (id === 'current') {
    const p = players.find((x) => x.playerid === url.searchParams.get('player'));
    const cur = p?.queue[p.index];
    if (cur?.remote) { id = 'radio'; label = cur.remote_title || 'Radio'; }
    else if (cur) { id = cur.coverid; }
    else { res.writeHead(404); res.end(); return; }
  }
  const al = albumById.get(+String(id).replace(/^c/, ''));
  if (al) label = al.album;
  if (id === '0' || (!al && id !== 'radio')) { res.writeHead(404); res.end(); return; }
  res.writeHead(200, { 'Content-Type': 'image/svg+xml', 'Cache-Control': 'max-age=300' });
  res.end(coverSvg(id, label));
}

function serveStatic(url, res) {
  let rel = decodeURIComponent(url.pathname);
  if (rel.startsWith('/web/')) rel = rel.slice(4);
  else if (rel === '/web') { res.writeHead(301, { Location: '/web/' }); res.end(); return; }
  if (rel === '/' || rel === '') rel = '/index.html';
  const file = path.normalize(path.join(WEB_DIR, rel));
  if (!file.startsWith(WEB_DIR + path.sep)) { res.writeHead(403); res.end(); return; }
  fs.readFile(file, (err, data) => {
    if (err) { res.writeHead(404, { 'Content-Type': 'text/plain' }); res.end('not found'); return; }
    res.writeHead(200, { 'Content-Type': MIME[path.extname(file)] || 'application/octet-stream', 'Cache-Control': 'no-cache' });
    res.end(data);
  });
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://localhost');
  if (url.pathname === '/jsonrpc.js' && req.method === 'POST') {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => {
      let reqJson;
      try { reqJson = JSON.parse(body); } catch { res.writeHead(400); res.end('bad json'); return; }
      const [pid, cmd] = reqJson.params || [];
      let out;
      try {
        const result = handle(pid, (cmd || []).map((x) => (typeof x === 'number' ? String(x) : x)));
        out = { id: reqJson.id, method: 'slim.request', params: reqJson.params, result };
      } catch (e) {
        out = { id: reqJson.id, method: 'slim.request', params: reqJson.params, error: { message: e.message } };
      }
      if (VERBOSE) console.log(pid || '-', JSON.stringify(cmd), out.error ? 'ERR ' + out.error.message : '');
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(out));
      setTimeout(publishChanges, 20);
    });
    return;
  }
  if (url.pathname.startsWith('/api/v1/')) return serveApi(req, res, url);
  if (url.pathname.startsWith('/music/')) return serveMusic(url, res);
  if (url.pathname.startsWith('/contributor/')) {
    const ar = artists.find((a) => a.portraitid === decodeURIComponent(url.pathname.split('/')[2]));
    res.writeHead(200, { 'Content-Type': 'image/svg+xml', 'Cache-Control': 'max-age=300' });
    res.end(coverSvg(ar?.portraitid || 'none', ar?.artist || ''));
    return;
  }
  if (url.pathname.startsWith('/imageproxy/') || url.pathname.startsWith('/html/images/')) { res.writeHead(200, { 'Content-Type': 'image/svg+xml' }); res.end(coverSvg('radio', 'Example FM')); return; }
  serveStatic(url, res);
});

server.listen(PORT, () => {
  console.log(`RMS web mock: http://localhost:${PORT}/  (also /web/)  players=${players.length}`);
});
