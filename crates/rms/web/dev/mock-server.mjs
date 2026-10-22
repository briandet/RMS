#!/usr/bin/env node
// Development-only mock of the RMS native API (/api/v1). NOT loaded by the web app.
//
//   node crates/rms/web/dev/mock-server.mjs            # http://localhost:9000/
//   PORT=9001 PLAYERS=0 node .../mock-server.mjs        # no players (empty state)
//   SPOTIFY=0 node .../mock-server.mjs                  # server without Spotify support
//
// Serves the web directory at / and /web/, answers /api/v1/* (including the event stream)
// with fake but plausible data shaped like tests/golden/native_api.json, simulates playback
// progress, and renders SVG cover art for /music/* and /contributor/*.

import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const WEB_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const PORT = +(process.env.PORT || 9000);
const NUM_PLAYERS = process.env.PLAYERS === undefined ? 2 : +process.env.PLAYERS;
const SPOTIFY = process.env.SPOTIFY !== '0';
const VERBOSE = !!process.env.VERBOSE;

// ------------------------------------------------------------------ fake library
let rnd = 42;
const rand = () => ((rnd = (rnd * 1103515245 + 12345) % 2147483648) / 2147483648);
const pick = (a) => a[Math.floor(rand() * a.length)];

const FIRST = ['Amber', 'Blue', 'Crimson', 'Silver', 'Northern', 'Velvet', 'Electric', 'Quiet', 'Golden', 'Midnight', 'Paper', 'Glass', 'Hollow', 'Neon', 'Wild'];
const SECOND = ['Foxes', 'Harbor', 'Lanterns', 'Orchard', 'Machines', 'Sparrows', 'Tides', 'Pilots', 'Atlas', 'Echoes', 'Rivers', 'Satellites', 'Wolves', 'Gardens'];
const WORDS = ['Light', 'Run', 'Summer', 'Ghost', 'Home', 'Fire', 'Stay', 'Ocean', 'City', 'Dream', 'Static', 'Gravity', 'Signal', 'Winter', 'Heart', 'Road', 'Morning', 'Shadows', 'Wave', 'Paris'];
const GENRES = ['Rock', 'Jazz', 'Electronic', 'Folk', 'Classical', 'Hip-Hop', 'Ambient', 'Pop'];

const genres = GENRES.map((g, i) => ({ id: i + 1, name: g }));
const artists = [];
const albums = [];
const tracks = [];
const seen = new Set();
while (artists.length < 45) {
  const name = `${pick(FIRST)} ${pick(SECOND)}`;
  if (seen.has(name)) continue;
  seen.add(name);
  const id = artists.length + 1;
  // every third artist has a picture
  artists.push({ id, name, ...(id % 3 === 0 ? { portrait: `contributor/p${id}/image` } : {}) });
}
artists.sort((a, b) => a.name.localeCompare(b.name));
for (const ar of artists) {
  const n = 1 + Math.floor(rand() * 5);
  for (let k = 0; k < n; k++) {
    const id = albums.length + 1;
    const title = `${pick(WORDS)} ${pick(['of', 'in', 'and', 'for'])} ${pick(WORDS)}`;
    // a few albums without a year ("Unknown year")
    const year = id % 13 === 0 ? 0 : 1968 + Math.floor(rand() * 57);
    const genre = pick(genres);
    const album = { id, title, artist: ar.name, artist_id: ar.id, year, genre_id: genre.id, added: rand(), tracks: [] };
    const nt = 6 + Math.floor(rand() * 9);
    const discs = id % 7 === 0 ? 2 : 1; // some double albums
    for (let t = 1; t <= nt; t++) {
      const tid = tracks.length + 1;
      const disc = discs > 1 ? (t <= Math.ceil(nt / 2) ? 1 : 2) : undefined;
      const track = {
        id: tid, title: `${pick(WORDS)}${rand() < .4 ? ' ' + pick(WORDS) : ''}`, artist: ar.name, artist_id: ar.id,
        album: title, album_id: id, duration: 120 + Math.floor(rand() * 300),
        tracknum: disc === 2 ? t - Math.ceil(nt / 2) : t, disc, year, genre: genre.name,
        coverid: `c${id}`, url: `file:///music/${encodeURIComponent(ar.name)}/${encodeURIComponent(title)}/${t}.flac`, type: 'flc',
      };
      tracks.push(track);
      album.tracks.push(track);
    }
    albums.push(album);
  }
}
albums.sort((a, b) => a.title.localeCompare(b.title));
const albumById = new Map(albums.map((a) => [a.id, a]));
const trackById = new Map(tracks.map((t) => [t.id, t]));

const playlists = [
  { id: 9001, name: 'Morning Coffee', trackIds: tracks.slice(3, 21).map((t) => t.id) },
  { id: 9002, name: 'Road Trip', trackIds: tracks.filter((_, i) => i % 7 === 0).slice(0, 40).map((t) => t.id) },
  { id: 9003, name: 'Empty List', trackIds: [] },
];

// Favorites as BrowseItems (ids like the real server: "0", "1", "1.0", …).
const favorites = [
  { id: '0', name: 'Example FM', type: 'audio', playable: true, url: 'http://radio.example.com/stream.mp3', artwork: 'imageproxy/radio/image' },
  { id: '1', name: 'Chill', type: 'folder', playable: false, items: [
    { id: '1.0', name: 'Ambient Radio', type: 'audio', playable: true, url: 'http://radio.example.com/ambient' },
    { id: '1.1', name: tracks[5].title + ' – ' + tracks[5].artist, type: 'audio', playable: true, url: tracks[5].url },
  ] },
];
const allFavorites = () => [...favorites, ...favorites.flatMap((f) => f.items || [])];

// Folder tree: root -> artist folders -> album folders -> tracks
const folders = new Map(); // id -> {id, name, type, children:[...]}
let folderSeq = 50000;
const rootFolder = { id: 0, children: [] };
for (const ar of artists.slice(0, 20)) {
  const f = { id: ++folderSeq, name: ar.name, type: 'folder', children: [] };
  for (const al of albums.filter((a) => a.artist_id === ar.id)) {
    const af = { id: ++folderSeq, name: al.title, type: 'folder', children: al.tracks.map((t) => ({ id: t.id, name: `${String(t.tracknum).padStart(2, '0')} ${t.title}.flac`, type: 'track' })) };
    af.children.push({ id: ++folderSeq, name: 'cover.txt', type: 'other' });
    folders.set(af.id, af);
    f.children.push(af);
  }
  folders.set(f.id, f);
  rootFolder.children.push(f);
}
rootFolder.children.push({ id: 9001, name: 'mix.m3u', type: 'playlist' });
folders.set(0, rootFolder);

// Spotify: a tiny menu tree; signed out until a login with any code but "bad".
const spotify = { signedIn: false, user: 'mockuser' };
const spotifyTop = [
  { id: '0', name: 'Search', type: 'search', playable: false },
  { id: '1', name: 'My Playlists', type: 'folder', playable: false },
  { id: '2', name: 'Spotify Connect works with the official apps', type: 'text', playable: false },
];
const spotifyLearned = [
  ['Signal Fire', 'Aurora Lane', 'Coastlines', 2019, 214, 122.1, '8B', -8.4, ['synth-pop'], 'Synth-pop', false],
  ['Paper Moon', 'The Velvet Hours, June Park', 'Paper Moon', 2021, 187, 96.0, '5A', -10.2, ['indie pop', 'dream pop'], 'Dream Pop', true],
  ['Northern Lights', 'Kestrel', 'Polar', 2016, 341, 174.0, '11A', -6.9, ['drum and bass'], 'Drum n Bass', false],
  ['Glass Harbor', 'Mira Sol', 'Tidewater', 2023, 256, 84.5, '3B', -12.8, [], 'Downtempo', false],
  ['Copper Sky', 'Low Orbit', 'Copper Sky', 2012, 402, 128.0, '9A', -7.5, ['techno', 'minimal techno'], 'Minimal', true],
].map(([title, artist, album, year, duration, bpm, key, loudness, genres, style, in_library], i) => ({
  url: `spotify://track:learned${i}`, title, artist, album, year, duration, bpm, key, loudness, genres, style, in_library,
  artwork: `imageproxy/sp${i % 3}/image`, learned_at: Math.floor(Date.now() / 1000) - i * 86400 * 3,
}));
const spotifyPlaylists = ['Discover Weekly', 'Release Radar', 'Liked Songs'].map((name, i) => ({ id: `1.${i}`, name, type: 'folder', playable: true, artwork: `imageproxy/sp${i}/image` }));
function spotifyTracks(prefix, names) {
  return names.map((name, i) => ({ id: `${prefix}.${i}`, name, type: 'audio', playable: true, url: `spotify://track:mock${prefix}${i}` }));
}
function spotifyMenu(id) {
  if (!id) return spotifyTop;
  if (id === '1') return spotifyPlaylists;
  if (/^1\.\d+$/.test(id)) return spotifyTracks(id, ['Signal Fire', 'Paper Moon', 'Northern Lights', 'Glass Harbor']);
  const m = /^0_(.*)$/.exec(id);
  if (m) {
    const term = decodeURIComponent(m[1]);
    return spotifyTracks(id, [`${term} (Radio Edit)`, `${term} – Live`, `Best of ${term}`]);
  }
  return null;
}
const spotifyItem = (id) => {
  const parent = id.includes('.') ? id.slice(0, id.lastIndexOf('.')) : '';
  return (spotifyMenu(parent) || []).find((x) => x.id === id);
};

// Internet radio: the providers, each a small menu; ids lead with the provider's tag.
const radioStations = (prefix, names) => names.map((name, i) => ({ id: `${prefix}.${i}`, name, type: 'audio', playable: true, url: `http://radio.example/${encodeURIComponent(name)}` }));
function radioMenu(id) {
  if (!id) return [{ id: 'radiodirectory', name: 'Popular Stations', type: 'folder', playable: false }, { id: 'radiobrowser', name: 'Radio Browser', type: 'folder', playable: false }];
  if (id === 'radiodirectory') return [{ id: 'radiodirectory.0', name: 'SomaFM', type: 'folder', playable: false }];
  if (id === 'radiodirectory.0') return radioStations(id, ['Groove Salad', 'Drone Zone', 'Secret Agent']);
  if (id === 'radiobrowser') return [{ id: 'radiobrowser.0', name: 'Search', type: 'search', playable: false }, { id: 'radiobrowser.1', name: 'Popular', type: 'folder', playable: false }];
  if (id === 'radiobrowser.1') return radioStations(id, ['France Info', 'BBC World Service', 'KEXP 90.3 FM']);
  const m = /^radiobrowser\.0_(.*)$/.exec(id);
  if (m) return radioStations(id, [`${decodeURIComponent(m[1])} FM`, `Radio ${decodeURIComponent(m[1])}`]);
  return null;
}
const radioItem = (id) => (radioMenu(id.slice(0, id.lastIndexOf('.'))) || []).find((x) => x.id === id);

// ------------------------------------------------------------------ server state
const settings = {
  library_name: 'Mock Library', media_dirs: ['/srv/music', '/mnt/nas/flac'], watch_folders: true, upnp_players: false, chromecast_players: false, airplay_players: false, art_folder: '', online_artist_images: false,
  spotify_bitrate: 320, spotify_normalisation: false,
  musicbrainz: false, acoustid_key: '', musicbrainz_identified: 0, musicbrainz_pending: 0,
  listenbrainz: false, discovered: 0, discovered_played: 0, discovered_rejected: 0,
  listenbrainz_accounts: [], scrobble_spotify: false, scrobble_pending: 0,
  effnet: false, effnet_status: 'off', effnet_analyzed: 0, effnet_pending: 0,
  visualizer_available: true, visualizer_encoder: 'h264_vaapi',
  visualizer_size: { value: '1920x1080', options: [['1920x1080', '1080p (1920×1080)'], ['1600x900', '900p (1600×900)'], ['1280x720', '720p (1280×720)']].map(([value, label]) => ({ value, label })) },
  visualizer_render_size: { value: '', options: [['', 'Automatic'], ['1600x900', '1600×900'], ['1280x720', '1280×720'], ['960x540', '960×540']].map(([value, label]) => ({ value, label })) },
  visualizer_fps: { value: '30', options: ['24', '30', '60'].map((value) => ({ value, label: value })) },
  visualizer_quality: { value: 30, min: 18, max: 45 },
  visualizer_preset_seconds: { value: 30, min: 5, max: 600 },
  visualizer_receiver_app: '',
};
const scan = { running: false, start: 0, total: tracks.length };
let lastscan = Math.floor(Date.now() / 1000) - 3600 * 5;

const DSTM = [['0', 'Disabled'], ['Example FM', 'Example FM'], ['PLUGIN_RANDOM_ALBUM_ITEM', 'Random Album'],
  ['PLUGIN_RANDOM_CONTRIBUTOR_ITEM', 'Random Artist'], ['RMS_DSTM_SIMILAR', 'Similar Songs'], ['PLUGIN_RANDOM_TRACK', 'Song Mix']];
const PLAYING_MODES = ['Nothing', 'Elapsed Time', 'Remaining Time', 'Small VU', 'Small VU and Elapsed Time', 'Small VU and Remaining Time',
  'Small Spectrum', 'Small Spectrum and Elapsed Time', 'Small Spectrum and Remaining Time', 'Spectrum Analyzer',
  'Spectrum Analyzer and Elapsed Time', 'Spectrum Analyzer and Remaining Time'];
const TEXT_SIZES = ['Light', 'Standard', 'Full'];
const BRIGHTNESS = [['4', '4 (Brightest)'], ['3', '3'], ['2', '2'], ['1', '1 (Dimmest)'], ['0', '0 (Dark)']];
const DISPLAY_CHOICES = {
  playing_mode: PLAYING_MODES.map((l, i) => [String(i), l]),
  text_size: TEXT_SIZES.map((l, i) => [String(i), l]),
  text_size_off: TEXT_SIZES.map((l, i) => [String(i), l]),
  brightness_on: BRIGHTNESS, brightness_off: BRIGHTNESS, brightness_idle: BRIGHTNESS,
};
const SCREEN_MODELS = new Set(['squeezebox2', 'squeezebox3', 'transporter', 'boom']);

function makePlayer(i) {
  const mac = `aa:bb:cc:00:00:0${i + 1}`;
  // PLAYERS=7: one of every kind (the web UI groups players by kind)
  const model = ['squeezelite', 'squeezebox3', 'boom', 'chromecast', 'airplay', 'upnp', 'browser'][i] || 'squeezelite';
  const p = {
    playerid: mac, name: ['Living Room', 'Kitchen', 'Office', 'Bedroom TV', 'JBL Boombox 3', 'Living Room (UPnP)', 'Chrome on macOS'][i] || `Player ${i + 1}`,
    model, modelname: ['SqueezeLite', 'Squeezebox Classic', 'Squeezebox Boom', 'Chromecast', 'AirPlay', 'UPnP renderer', 'Web browser'][i] || 'SqueezeLite',
    // of the extra kinds, the AirPlay speaker is off and the UPnP renderer gone
    ip: `192.168.1.${20 + i}`, connected: i !== 5, power: i !== 4,
    mode: 'stop', queue: [], index: 0, elapsedBase: 0, startedAt: 0,
    volume: 40, muted: false, shuffle: 0, repeat: 0, timestamp: Date.now(),
    settings: { max_bitrate: 0, digital_volume_control: true, play_delay: 0 },
    syncGroup: null,
  };
  if (SCREEN_MODELS.has(model)) {
    Object.assign(p.settings, { playing_mode: '5', text_size: '1', text_size_off: '1', brightness_on: '4', brightness_off: '1', brightness_idle: '2' });
    if (model === 'boom') Object.assign(p.settings, { auto_brightness_min: 1, auto_brightness_sensitivity: 50 });
  }
  return p;
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
function play(p, at = 0) { p.mode = 'play'; p.elapsedBase = at; p.startedAt = Date.now(); p.power = true; }
function setIndex(p, i) {
  if (!p.queue.length) { p.mode = 'stop'; return; }
  p.index = ((i % p.queue.length) + p.queue.length) % p.queue.length;
  play(p, 0);
}
function touch(p) { p.timestamp = Date.now(); }

// Advance tracks that have finished.
function advance(p) {
  const cur = p.queue[p.index];
  if (p.mode !== 'play' || !cur || !cur.duration) return;
  const e = elapsedOf(p) - cur.duration;
  if (e < 0) return;
  if (p.repeat === 1) { play(p, e); return; }
  if (p.index + 1 >= p.queue.length) {
    if (p.repeat === 2) { p.index = 0; play(p, e); return; }
    p.mode = 'stop'; p.elapsedBase = 0; return;
  }
  p.index++;
  play(p, e);
}

// ------------------------------------------------------------------ helpers
class HttpError extends Error {
  constructor(status, message) { super(message); this.status = status; }
}
const bad = (msg) => new HttpError(400, msg);
const notFound = (what) => new HttpError(404, `no such ${what}`);

/** Page<T> from a full list and the request's offset/limit. */
function page(list, q, map = (x) => x) {
  const offset = +q.get('offset') || 0;
  const limit = q.has('limit') ? Math.min(+q.get('limit'), 10000) : 100;
  return { total: list.length, offset, items: list.slice(offset, offset + limit).map(map) };
}
/** All words of `q` occur in `text`. */
function matches(text, q) {
  const words = String(q || '').toLowerCase().split(/\s+/).filter(Boolean);
  const t = String(text).toLowerCase();
  return words.every((w) => t.includes(w));
}
const choice = (pairs, value) => ({ value: String(value), options: pairs.map(([v, label]) => ({ value: v, label })) });
// playback: the same for every player
Object.assign(settings, {
  transition_type: 0, transition_duration: 5, replay_gain_mode: 0,
  dont_stop_the_music: choice(DSTM, '0'), adventurous: 15, scrobble_to: [],
});
const letter = (s) => (s[0] || '').toUpperCase();

const artistOut = (a) => ({ id: a.id, name: a.name, ...(a.portrait ? { portrait: a.portrait } : {}), letter: letter(a.name) });
const albumOut = (a, withLetter = 'title') => ({
  id: a.id, title: a.title, artist: a.artist, artist_id: a.artist_id, ...(a.year ? { year: a.year } : {}), compilation: false,
  release_type: ['album', 'album', 'album', 'ep', 'single'][a.id % 5],
  artwork: `music/c${a.id}/cover`, ...(withLetter ? { letter: letter(withLetter === 'artist' ? a.artist : a.title) } : {}),
});

function trackNative(t) {
  if (t.remote) {
    const songs = ['Northern Lights', 'Signal Fire', 'Paper Moon'];
    const song = songs[Math.floor(Date.now() / 30000) % songs.length];
    return { url: t.url, title: song, artist: 'DJ Mock', artwork: 'imageproxy/radio/image', remote: true, station: t.remote_title };
  }
  return {
    id: t.id, url: t.url, title: t.title, artist: t.artist, artist_id: t.artist_id, album: t.album, album_id: t.album_id,
    duration: t.duration, track_number: t.tracknum, ...(t.disc ? { disc: t.disc } : {}), ...(t.year ? { year: t.year } : {}),
    artwork: `music/${t.coverid}/cover`, remote: false, format: t.type,
  };
}

function radioTrack(url, name) {
  return { id: -Date.now(), title: name || 'Stream', url, remote: true, duration: 0, remote_title: name };
}

function folderTracks(f) {
  const out = [];
  const walk = (x) => { for (const c of x.children || []) { if (c.type === 'track') out.push(trackById.get(c.id)); else if (c.type === 'folder') walk(c); } };
  walk(f);
  return out.filter(Boolean);
}

/** The tracks an enqueue / mix item stands for. */
function itemTracks(it) {
  switch (it?.type) {
    case 'track': return [trackById.get(+it.id)].filter(Boolean);
    case 'album': return albumById.get(+it.id)?.tracks || [];
    case 'artist': return albums.filter((a) => a.artist_id === +it.id).flatMap((a) => a.tracks);
    case 'genre': return albums.filter((a) => a.genre_id === +it.id).flatMap((a) => a.tracks);
    case 'year': return albums.filter((a) => a.year === +it.year).flatMap((a) => a.tracks);
    case 'playlist': return (playlists.find((x) => x.id === +it.id)?.trackIds || []).map((id) => trackById.get(id));
    case 'folder': { const f = folders.get(+it.id); return f ? folderTracks(f) : []; }
    case 'url': return [tracks.find((x) => x.url === it.url) || radioTrack(it.url, 'Stream')];
    case 'favorite': {
      const f = allFavorites().find((x) => x.id === it.id);
      if (!f) throw notFound('favorite');
      return [tracks.find((x) => x.url === f.url) || radioTrack(f.url, f.name)];
    }
    case 'spotify': {
      const f = spotifyItem(it.id);
      if (!f) throw notFound('Spotify item');
      const list = f.type === 'folder' ? spotifyMenu(f.id) : [f];
      return list.map((x) => ({ ...radioTrack(x.url, x.name), duration: 200 }));
    }
    case 'radio': {
      const f = radioItem(it.id);
      if (!f) throw notFound('radio item');
      return (f.type === 'folder' ? radioMenu(f.id) : [f]).map((x) => radioTrack(x.url, x.name));
    }
    default: throw bad(`unknown item type ${it?.type}`);
  }
}

function loadInto(p, list, mode, start = 0) {
  const items = list.map((t) => ({ ...t }));
  if (mode === 'play') { p.queue = items; p.index = Math.min(+start || 0, Math.max(0, items.length - 1)); if (items.length) play(p, 0); else p.mode = 'stop'; }
  else if (mode === 'add') { p.queue.push(...items); if (p.mode === 'stop' && p.queue.length === items.length) p.index = 0; }
  else if (mode === 'next') p.queue.splice(p.index + 1, 0, ...items);
  touch(p);
}

// ------------------------------------------------------------------ native API (/api/v1)
function serverOut() {
  if (scan.running && Date.now() - scan.start > 12000) { scan.running = false; lastscan = Math.floor(Date.now() / 1000); }
  const r = {
    name: settings.library_name, version: '0.1.0-mock', uuid: 'mock', last_scan: lastscan,
    library: { albums: albums.length, artists: artists.length, genres: genres.length, tracks: tracks.length, duration: tracks.reduce((s, t) => s + t.duration, 0) },
  };
  if (scan.running) {
    const frac = Math.min(1, (Date.now() - scan.start) / 12000);
    r.scan = { step: frac < .7 ? 'directory' : 'discovering_files', done: Math.floor(frac * scan.total), total: scan.total };
  }
  return r;
}

function playerOut(p) {
  advance(p);
  const cur = p.queue[p.index];
  return {
    id: p.playerid, name: p.name, model: p.model, model_name: p.modelname, ip: p.ip, firmware: 'mock',
    connected: p.connected, power: p.power, mode: p.mode, buffering: false,
    elapsed: cur ? Math.round(elapsedOf(p) * 1000) / 1000 : 0,
    duration: cur?.duration || undefined, can_seek: !!cur && !cur.remote,
    volume: p.volume, muted: p.muted, volume_fixed: !p.settings.digital_volume_control,
    shuffle: ['off', 'songs', 'albums'][p.shuffle], repeat: ['off', 'one', 'all'][p.repeat],
    queue: { version: p.timestamp, length: p.queue.length, index: p.queue.length ? p.index : undefined },
    now_playing: cur ? trackNative(cur) : undefined,
    synced_with: p.syncGroup ? players.filter((x) => x !== p && x.syncGroup === p.syncGroup).map((x) => x.playerid) : [],
  };
}

function playerSettingsOut(p) {
  const s = p.settings;
  const r = {
    play_delay: s.play_delay ?? 0, max_bitrate: s.max_bitrate, digital_volume_control: s.digital_volume_control,
  };
  if (p.model === 'chromecast') {
    r.visualizer = { value: s.visualizer ?? 'auto', options: [['auto', 'Automatic (drawn on the TV)'], ['1', 'Video from the server'], ['tv', 'Drawn on the TV'], ['0', 'Off (the cover)']].map(([value, label]) => ({ value, label })) };
  }
  if (SCREEN_MODELS.has(p.model)) {
    r.display = Object.fromEntries(Object.entries(DISPLAY_CHOICES).map(([k, pairs]) => [k, choice(pairs, s[k])]));
    if (s.auto_brightness_min != null) {
      r.display.auto_brightness_min = { value: s.auto_brightness_min, min: 0, max: 7 };
      r.display.auto_brightness_sensitivity = { value: s.auto_brightness_sensitivity, min: 0, max: 100 };
    }
  }
  return r;
}

function patchPlayerSettings(p, b) {
  const s = p.settings;
  for (const [k, v] of Object.entries(b)) {
    if (k === 'max_bitrate') s[k] = +v;
    else if (k === 'play_delay') {
      if (!(v >= 0 && v <= 1000)) throw bad('play_delay must be 0–1000');
      s[k] = +v;
    }
    else if (k === 'digital_volume_control') s[k] = !!v;
    else if (k === 'visualizer') {
      if (p.model !== 'chromecast') throw bad('this player has no visualizer');
      if (!['auto', '1', 'tv', '0'].includes(v)) throw bad(`${v} is not a valid visualizer`);
      s[k] = v;
    }
    else if (k in DISPLAY_CHOICES || k.startsWith('auto_brightness')) {
      if (!(k in s)) throw bad(`this player has no ${k}`);
      if (k in DISPLAY_CHOICES && !DISPLAY_CHOICES[k].some(([x]) => x === String(v))) throw bad(`unknown ${k} ${v}`);
      s[k] = k in DISPLAY_CHOICES ? String(v) : +v;
    } else throw bad(`unknown field \`${k}\``);
  }
  return playerSettingsOut(p);
}

function patchServerSettings(b) {
  for (const [k, v] of Object.entries(b)) {
    if (!(k in settings)) throw bad(`unknown field \`${k}\``);
    if (k === 'spotify_bitrate' && ![96, 160, 320].includes(v)) throw bad('spotify_bitrate must be 96, 160 or 320');
    if (k === 'media_dirs' && !Array.isArray(v)) throw bad('media_dirs must be a list');
    if (k === 'visualizer_receiver_app') {
      const id = String(v).trim().toUpperCase();
      if (id && !/^[0-9A-F]{8}$/.test(id)) throw bad('visualizer_receiver_app: a Cast app id is 8 hex digits, like 1A2B3C4D');
      settings[k] = id;
      continue;
    }
    if (k === 'transition_type' && !(v >= 0 && v <= 4)) throw bad('transition_type must be 0–4');
    if (k === 'dont_stop_the_music') {
      if (!DSTM.some(([x]) => x === v)) throw bad(`${v} is not a valid dont_stop_the_music`);
      settings[k] = choice(DSTM, v);
      continue;
    }
    if (k === 'scrobble_to') {
      const u = v.find((x) => !settings.listenbrainz_accounts.some((a) => a.user === x));
      if (u) throw bad(`scrobble_to: no ListenBrainz account ${u}`);
    }
    if (k.startsWith('visualizer_')) {
      const c = settings[k];
      if (c.options && !c.options.some((o) => o.value === v)) throw bad(`${v} is not a valid ${k}`);
      if (c.min != null && !(v >= c.min && v <= c.max)) throw bad(`${k} must be ${c.min}–${c.max}`);
      c.value = v;
      continue;
    }
    settings[k] = v;
  }
  return settings;
}

function nativeCommand(p, b) {
  const inQueue = (i) => { if (!(i >= 0 && i < p.queue.length)) throw bad(`index ${i} is out of range`); };
  advance(p);
  switch (b.command) {
    case 'play': if (p.queue.length && p.mode !== 'play') play(p, p.mode === 'pause' ? p.elapsedBase : 0); return {};
    case 'pause': if (p.mode === 'play') { p.elapsedBase = elapsedOf(p); p.mode = 'pause'; } return {};
    case 'toggle_pause': return nativeCommand(p, { command: p.mode === 'play' ? 'pause' : 'play' });
    case 'stop': p.mode = 'stop'; p.elapsedBase = 0; return {};
    case 'next': setIndex(p, p.index + 1); return {};
    case 'previous': setIndex(p, p.index - 1); return {};
    case 'seek': { const t = Math.max(0, +b.position || 0); if (p.mode === 'play') play(p, t); else p.elapsedBase = t; return {}; }
    case 'volume': p.volume = Math.max(0, Math.min(100, +b.value)); p.muted = false; return {};
    case 'mute': p.muted = b.muted == null ? !p.muted : !!b.muted; return {};
    case 'power': p.power = !!b.on; if (!p.power && p.mode === 'play') { p.elapsedBase = elapsedOf(p); p.mode = 'pause'; } return {};
    case 'shuffle': p.shuffle = ['off', 'songs', 'albums'].indexOf(b.mode); touch(p); return {};
    case 'repeat': p.repeat = ['off', 'one', 'all'].indexOf(b.mode); return {};
    case 'jump': inQueue(b.index); setIndex(p, b.index); return {};
    case 'move': {
      inQueue(b.from); inQueue(b.to);
      const curTrack = p.queue[p.index];
      const [t] = p.queue.splice(b.from, 1);
      p.queue.splice(b.to, 0, t);
      p.index = p.queue.indexOf(curTrack);
      touch(p);
      return {};
    }
    case 'remove': {
      const i = b.index;
      inQueue(i);
      p.queue.splice(i, 1);
      if (i < p.index) p.index--;
      else if (i === p.index) { if (p.index >= p.queue.length) { p.index = 0; p.mode = 'stop'; } else if (p.mode === 'play') play(p, 0); }
      if (!p.queue.length) p.mode = 'stop';
      touch(p);
      return {};
    }
    case 'clear': p.queue = []; p.index = 0; p.mode = 'stop'; p.elapsedBase = 0; touch(p); return {};
    case 'enqueue': {
      const mode = b.mode || 'play';
      const list = itemTracks(b.item);
      if (mode === 'play') p.power = true;
      loadInto(p, list, mode, b.start);
      return { count: list.length };
    }
    case 'mix': {
      const seed = itemTracks(b.seed);
      const genre = seed[0]?.genre;
      const list = [...seed.slice(0, 1), ...tracks.filter((t) => t.genre === genre && t !== seed[0]).slice(0, 19)];
      loadInto(p, list, 'play');
      return { count: list.length };
    }
    case 'sync': {
      const o = players.find((x) => x.playerid === b.with);
      if (!o) throw bad(`unknown player ${b.with}`);
      const g = p.syncGroup || o.syncGroup || `g${Date.now()}`;
      p.syncGroup = g; o.syncGroup = g;
      return {};
    }
    case 'unsync': {
      p.syncGroup = null;
      for (const x of players) if (x.syncGroup && players.filter((y) => y.syncGroup === x.syncGroup).length < 2) x.syncGroup = null;
      return {};
    }
    case 'rename': if (b.name) p.name = String(b.name); return {};
    default: throw bad(`unknown command ${b.command}`);
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

/** Route one /api/v1 request; returns the response body or throws an HttpError. */
function route(method, parts, q, body) {
  const [r0, r1, r2] = parts;
  const id = r1 === undefined ? undefined : r1;
  const GET = method === 'GET';
  switch (r0) {
    case 'server': return serverOut();
    case 'players': {
      if (!id) return players.map(playerOut);
      const p = players.find((x) => x.playerid === id);
      if (!p) throw new HttpError(404, `unknown player ${id}`);
      if (!r2) return playerOut(p);
      if (r2 === 'queue') {
        const offset = +q.get('offset') || 0;
        const limit = q.has('limit') ? +q.get('limit') : Infinity;
        return { version: p.timestamp, length: p.queue.length, index: p.queue.length ? p.index : undefined, offset, items: p.queue.slice(offset, offset + limit).map(trackNative) };
      }
      if (r2 === 'spotify-queue' && GET) {
        const cur = p.queue[p.index];
        if (!cur?.url?.startsWith('spotify://connect:')) return { connect: false, items: [] };
        return { connect: true, items: spotifyTracks('9', ['Glass Harbor', 'Northern Lights', 'Paper Moon']).map((t, i) => ({ url: t.url, title: t.name, artist: 'Mock Artist', album: 'Mock Album', duration: 180 + i * 20, artwork: `imageproxy/sp${i}/image`, remote: true })) };
      }
      if (r2 === 'command' && method === 'POST') {
        const r = nativeCommand(p, body || {});
        setTimeout(publishChanges, 20);
        return r?.count != null ? { count: r.count } : {};
      }
      if (r2 === 'settings' && GET) return playerSettingsOut(p);
      if (r2 === 'settings' && method === 'PATCH') return patchPlayerSettings(p, body || {});
      break;
    }
    case 'artists': {
      if (id && r2 === 'top-tracks') return tracks.filter((t) => t.artist_id === +id).slice(0, 5).map(trackNative);
      if (id && r2 === 'similar') {
        return artists.filter((x) => x.id !== +id).slice(0, 5).map((x) => ({ name: x.name, artist: artistOut(x) }))
          .concat([{ name: 'Not In Library' }, { name: 'Someone Else' }]);
      }
      if (id) { const a = artists.find((x) => x.id === +id); if (!a) throw notFound('artist'); return artistOut(a); }
      return page(artists.filter((a) => matches(a.name, q.get('q'))), q, artistOut);
    }
    case 'albums': {
      if (id) {
        const a = albumById.get(+id);
        if (!a) throw notFound('album');
        const { letter: _, ...out } = albumOut(a);
        return { ...out, duration: a.tracks.reduce((s, t) => s + t.duration, 0), tracks: a.tracks.map(trackNative) };
      }
      const sort = q.get('sort') || 'title';
      let list = albums;
      if (q.has('artist')) list = list.filter((a) => a.artist_id === +q.get('artist'));
      if (q.has('genre')) list = list.filter((a) => a.genre_id === +q.get('genre'));
      if (q.has('year')) list = list.filter((a) => a.year === +q.get('year'));
      if (q.get('q')) list = list.filter((a) => matches(`${a.title} ${a.artist}`, q.get('q')));
      if (sort === 'added') list = [...list].sort((a, b) => b.added - a.added);
      else if (sort === 'artist') list = [...list].sort((a, b) => a.artist.localeCompare(b.artist) || a.year - b.year);
      else if (sort === 'year') list = [...list].sort((a, b) => a.year - b.year || a.title.localeCompare(b.title));
      else if (sort !== 'title') throw bad(`unknown sort "${sort}" (title, artist, year or added)`);
      return page(list, q, (a) => albumOut(a, sort === 'title' || sort === 'artist' ? sort : null));
    }
    case 'genres': return page(genres.filter((g) => matches(g.name, q.get('q'))), q);
    case 'years': return page([...new Set(albums.map((a) => a.year))].sort((a, b) => b - a).map((year) => ({ year })), q);
    case 'tracks': return page(tracks.filter((t) => matches(t.title, q.get('q'))), q, trackNative);
    case 'playlists': {
      const itemIds = (it) => {
        if (!it) return [];
        if (it.type === 'track') return [it.id];
        if (it.type === 'album') return (albumById.get(it.id)?.tracks || []).map((t) => t.id);
        if (it.type === 'playlist') return playlists.find((x) => x.id === it.id)?.trackIds.slice() || [];
        if (it.type === 'artist') return tracks.filter((t) => t.artist_id === it.id).map((t) => t.id);
        return [];
      };
      if (!id && method === 'POST') {
        const name = (body.name || '').trim();
        if (!name) throw new HttpError(400, 'a playlist needs a name');
        if (playlists.some((x) => x.name === name)) throw new HttpError(400, `there already is a playlist named ${name}`);
        const fromQueue = body.from_player ? (players.find((x) => x.playerid === body.from_player)?.queue || []).map((t) => t.id).filter((x) => x != null) : [];
        const pl = { id: Math.max(...playlists.map((x) => x.id), 9000) + 1, name, trackIds: body.item ? itemIds(body.item) : fromQueue };
        playlists.push(pl);
        return { id: pl.id, name: pl.name, editable: true };
      }
      if (!id) return page(playlists.filter((p) => matches(p.name, q.get('q'))), q, ({ id: pid, name }) => ({ id: pid, name, editable: true }));
      const pl = playlists.find((x) => x.id === +id);
      if (!pl) throw notFound('playlist');
      if (!r2) {
        if (method === 'PATCH') { pl.name = body.name.trim(); }
        if (method === 'DELETE') { playlists.splice(playlists.indexOf(pl), 1); return {}; }
        return { id: pl.id, name: pl.name, editable: true };
      }
      if (r2 === 'tracks') {
        if (method === 'PUT') { pl.trackIds = body.order.map((i) => pl.trackIds[i]); return {}; }
        if (method === 'POST') {
          const add = itemIds(body.item);
          pl.trackIds.splice(body.position ?? pl.trackIds.length, 0, ...add);
          return { count: add.length };
        }
        return page(pl.trackIds.map((t) => trackById.get(t)), q, trackNative);
      }
      break;
    }
    case 'folders': {
      const f = folders.get(+(q.get('id') || 0));
      if (!f) throw notFound('folder');
      return page(f.children, q, (c) => ({ type: c.type, id: c.id, name: c.name, ...(c.type === 'track' ? { track: trackNative(trackById.get(c.id)) } : {}) }));
    }
    case 'search': {
      const term = q.get('q');
      if (!term || !term.trim()) throw bad('q is missing');
      const sub = new URLSearchParams({ offset: q.get('offset') || '0', limit: q.get('limit') || '20' });
      return {
        artists: page(artists.filter((a) => matches(a.name, term)), sub, artistOut),
        albums: page(albums.filter((a) => matches(`${a.title} ${a.artist}`, term)), sub, (a) => albumOut(a)),
        tracks: page(tracks.filter((t) => matches(`${t.title} ${t.artist} ${t.album}`, term)), sub, trackNative),
        genres: page(genres.filter((g) => matches(g.name, term)), sub),
      };
    }
    case 'favorites': {
      if (GET && !id) {
        const fid = q.get('id');
        const list = fid ? allFavorites().find((f) => f.id === fid)?.items : favorites;
        if (!list) throw notFound('favorite');
        return page(list, q, ({ items, ...f }) => f);
      }
      if (method === 'POST' && !id) {
        if (!body?.url) throw bad('url must not be empty');
        const next = favorites.reduce((m, f) => Math.max(m, +f.id), -1) + 1;
        favorites.push({ id: String(next), name: body.title || body.url, type: 'audio', playable: true, url: body.url });
        return {};
      }
      if (method === 'DELETE' && id) {
        const top = favorites.findIndex((f) => f.id === id);
        if (top >= 0) { favorites.splice(top, 1); return {}; }
        for (const f of favorites) {
          const i = (f.items || []).findIndex((x) => x.id === id);
          if (i >= 0) { f.items.splice(i, 1); return {}; }
        }
        throw notFound('favorite');
      }
      break;
    }
    case 'listenbrainz': {
      if (!id && GET) {
        const top = settings.listenbrainz_accounts.flatMap((a) => [
          { id: `recs:${a.user}`, name: `Recommended for ${a.user}`, type: 'folder', playable: true },
          { id: 'pl:00000000-0000-0000-0000-000000000001', name: `Weekly Jams for ${a.user}`, type: 'folder', playable: true },
        ]);
        const leaf = ['Awake — Tycho', 'Song 2 — Blur', 'Bless This Morning Year — Helios']
          .map((name, i) => ({ id: `rec:${i}`, name, type: 'audio', playable: true }));
        return page(q.get('id') ? leaf : top, q);
      }
      if (id === 'accounts' && !r2 && method === 'POST') {
        const user = `mock-user-${settings.listenbrainz_accounts.length + 1}`;
        const a = { user, default: true };
        settings.listenbrainz_accounts.push(a);
        return a;
      }
      if (id === 'accounts' && r2) {
        const user = decodeURIComponent(r2);
        const a = settings.listenbrainz_accounts.find((x) => x.user === user);
        if (!a) throw notFound('account');
        if (method === 'DELETE') { settings.listenbrainz_accounts.splice(settings.listenbrainz_accounts.indexOf(a), 1); return {}; }
        if (method === 'PATCH') { Object.assign(a, body); return {}; }
      }
      break;
    }
    case 'radio': {
      if (!id && GET) {
        const list = radioMenu(q.get('id') || '');
        if (!list) throw notFound('radio item');
        return page(list, q);
      }
      break;
    }
    case 'spotify': {
      if (!SPOTIFY) throw new HttpError(404, 'Spotify is not available');
      if (!id && GET) return spotify.signedIn ? { signed_in: true, user: spotify.user, product: 'premium', learn: true, learned: spotifyLearned.length } : { signed_in: false, learn: true, learned: spotifyLearned.length };
      if (id === 'learned' && GET) return { items: spotifyLearned };
      if (id === 'learned' && method === 'DELETE') { spotifyLearned.length = 0; return {}; }
      if (id === 'items' && GET) {
        if (!spotify.signedIn) throw new HttpError(422, 'not signed in to Spotify');
        const list = spotifyMenu(q.get('id') || '');
        if (!list) throw notFound('Spotify item');
        return page(list, q);
      }
      if (id === 'login' && method === 'POST') {
        const code = body?.code?.trim();
        if (!code) return { url: 'https://accounts.spotify.com/authorize?client_id=mock&redirect_uri=http%3A%2F%2F127.0.0.1%3A5588%2Flogin' };
        if (code.includes('bad')) throw new HttpError(422, 'Spotify refused the sign-in code');
        spotify.signedIn = true;
        return { user: spotify.user };
      }
      if (id === 'logout' && method === 'POST') { spotify.signedIn = false; return {}; }
      break;
    }
    case 'settings': {
      if (GET) return settings;
      if (method === 'PATCH') return patchServerSettings(body || {});
      break;
    }
    case 'library': {
      if (id === 'scan' && method === 'POST') {
        const mode = body?.mode || 'changes';
        if (!['changes', 'playlists', 'full'].includes(mode)) throw bad(`unknown mode ${mode}`);
        scan.running = true; scan.start = Date.now();
        setTimeout(publishChanges, 20);
        return {};
      }
      break;
    }
    default: break;
  }
  throw new HttpError(404, 'no such resource');
}

function serveApi(req, res, url) {
  const json = (status, data) => { res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(data)); };
  const parts = url.pathname.split('/').slice(3).filter(Boolean).map(decodeURIComponent); // after /api/v1/
  if (parts[0] === 'events') return serveEvents(req, res);
  let raw = '';
  req.on('data', (c) => { raw += c; });
  req.on('end', () => {
    let body;
    if (raw) { try { body = JSON.parse(raw); } catch { json(400, { error: 'invalid JSON body' }); return; } }
    try {
      const out = route(req.method, parts, url.searchParams, body);
      if (VERBOSE) console.log(req.method, url.pathname + url.search, raw);
      json(200, out);
    } catch (e) {
      if (VERBOSE) console.log(req.method, url.pathname + url.search, raw, 'ERR', e.message);
      json(e.status || 500, { error: e.message });
    }
  });
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
  if (al) label = al.title;
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
  if (url.pathname.startsWith('/api/v1/')) return serveApi(req, res, url);
  if (url.pathname.startsWith('/music/')) return serveMusic(url, res);
  if (url.pathname.startsWith('/contributor/')) {
    const pid = decodeURIComponent(url.pathname.split('/')[2]);
    const ar = artists.find((a) => a.portrait === `contributor/${pid}/image`);
    res.writeHead(200, { 'Content-Type': 'image/svg+xml', 'Cache-Control': 'max-age=300' });
    res.end(coverSvg(pid || 'none', ar?.name || ''));
    return;
  }
  if (url.pathname.startsWith('/imageproxy/') || url.pathname.startsWith('/html/images/')) { res.writeHead(200, { 'Content-Type': 'image/svg+xml' }); res.end(coverSvg('radio', 'Example FM')); return; }
  serveStatic(url, res);
});

server.listen(PORT, () => {
  console.log(`RMS web mock: http://localhost:${PORT}/  (also /web/)  players=${players.length}`);
});
