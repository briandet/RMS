// What the analyses made of a library track: the file, bliss's sound features and nearest
// tracks, tempo, key and the energy over the song, the Chromaprint fingerprint and the same
// recording elsewhere, EffNet's styles and moods, and the MusicBrainz lookup, each with where it
// stands.  Route: #/library/track/<id>

import { getTrackAnalysis } from '../api.js';
import { h, icon, fmtTime } from '../util.js';
import { enc, bigActions, loadingBox } from './common.js';

/** @typedef {import('../api-types').TrackAnalysis} TrackAnalysis */
/** @typedef {import('../api-types').AnalysisStage<unknown>} Stage */

const STATUS = {
  done: 'Done',
  failed: 'Nothing came of it',
  stale: 'Older version, to be redone',
  pending: 'Not analysed yet',
  skipped: 'Not analysed',
  off: 'Turned off',
};
const WHY = {
  failed: 'Analysed, but the audio could not be read or gave nothing to go on (too short or silent). It is tried again when the file changes.',
  stale: 'These results are from an older version of the analysis; the background analysis redoes them.',
  pending: 'The background analysis has not reached this song yet.',
  skipped: 'Songs longer than 20 minutes, and streams, are not analysed.',
};

export async function trackPage(v, id) {
  const name = v.query.get('name') || 'Track';
  v.crumbs([{ label: 'Library', href: '#/library' }, { label: name }]);
  v.body.replaceChildren(loadingBox());
  /** @type {TrackAnalysis} */
  const a = await getTrackAnalysis(id);
  if (!v.alive) return;
  const title = a.title || name;
  // where it is in the library: Artists › its album's artist › the album › the track
  const al = a.album;
  const artistHref = al?.artist_id != null && al.artist ? `#/library/artist/${enc(al.artist_id)}?name=${enc(al.artist)}` : null;
  const albumHref = al ? `#/library/album/${enc(al.id)}?name=${enc(al.title)}` : null;
  v.crumbs([
    ...(artistHref ? [{ label: 'Artists', href: '#/library/artists' }, { label: al.artist, href: artistHref }]
      : [{ label: 'Albums', href: '#/library/albums' }]),
    ...(al ? [{ label: al.title, href: albumHref }] : []),
    { label: title },
  ]);

  v.body.replaceChildren(
    h('header', { class: 'page-head' },
      h('div', { class: 'page-head-text' },
        h('h1', {}, title),
        (a.artist || al) && h('div', { class: 'page-sub' }, a.artist, a.artist && al ? ' · ' : null,
          al && h('a', { href: albumHref }, al.title)),
        h('div', { class: 'page-actions' }, bigActions(v.ctx, 'track_id', a.track_id, title),
          h('button', { class: 'btn btn-ghost', type: 'button', onclick: () => v.ctx.reload() }, icon('refresh'), 'Refresh')))),
    h('div', { class: 'settings-grid analysis' },
      fileCard(a.file),
      stageCard('Sound (bliss)', 'How it sounds: what “similar songs” and mixes go by, and its loudness.', a.bliss, bliss),
      stageCard('Tempo and key', 'Beats, bars, key and how the energy rises and falls, for beat-matched mixes.', a.rhythm, rhythm),
      stageCard('Fingerprint (Chromaprint)', 'An audio fingerprint of the first two minutes, to recognise the same recording.', a.chromaprint, chromaprint),
      stageCard('Kind of music (EffNet)', 'Styles, moods, themes and instruments, as a network trained on Discogs hears them.', a.effnet, effnet),
      stageCard('MusicBrainz', 'The recording on MusicBrainz, and the genres it fills in.', a.musicbrainz, musicbrainz)));
}

// ---------------------------------------------------------------- sections

function card(heading, ...children) {
  return h('section', { class: 'settings-card' }, h('h2', {}, heading), ...children);
}

function facts(pairs) {
  const rows = pairs.filter(([, v]) => v != null && v !== '' && v !== false);
  return h('dl', { class: 'about' }, rows.flatMap(([k, v]) => [h('dt', {}, k), h('dd', {}, v)]));
}

function fileCard(f) {
  const audio = [
    f.format,
    f.lossless ? 'lossless' : null,
    f.bitrate ? `${Math.round(f.bitrate / 1000)} kbps${f.vbr ? ' VBR' : ''}` : null,
    f.sample_rate ? `${(f.sample_rate / 1000).toLocaleString()} kHz` : null,
    f.sample_size ? `${f.sample_size} bit` : null,
    f.channels ? (f.channels === 1 ? 'mono' : f.channels === 2 ? 'stereo' : `${f.channels} channels`) : null,
  ].filter(Boolean).join(' · ');
  return card('File', facts([
    ['Path', f.path ? h('code', { class: 'wrap' }, f.path) : h('code', { class: 'wrap' }, f.url)],
    ['Audio', audio],
    ['Size', f.size ? `${(f.size / 1e6).toFixed(1)} MB` : null],
    ['Length', f.duration != null ? fmtTime(f.duration) : null],
    ['Cue track', f.segment_start != null ? `${fmtTime(f.segment_start)} – ${f.segment_end != null ? fmtTime(f.segment_end) : 'end'}` : null],
    ['ReplayGain', f.replay_gain != null ? `${f.replay_gain.toFixed(2)} dB${f.replay_peak != null ? `, peak ${f.replay_peak.toFixed(3)}` : ''}` : null],
    ['BPM tag', f.bpm_tag],
    ['Audio id', f.fingerprint ? h('code', { class: 'wrap' }, f.fingerprint) : null],
  ]));
}

/** A section for one analysis: its status, and its results when done. */
function stageCard(heading, about, /** @type {Stage} */ s, body) {
  const when = s.analyzed_at ? new Date(s.analyzed_at * 1000).toLocaleString() : null;
  const meta = [
    s.version != null ? `version ${s.version}${s.version !== s.current_version ? ` (now ${s.current_version})` : ''}` : null,
    when && `analysed ${when}`,
  ].filter(Boolean).join(' · ');
  return card(heading,
    h('p', { class: 'muted small' }, about),
    h('p', { class: 'small' }, h('span', { class: `status-badge status-${s.status}` }, STATUS[s.status] || s.status),
      meta && h('span', { class: 'muted' }, ` ${meta}`)),
    WHY[s.status] && h('p', { class: 'muted small' }, WHY[s.status]),
    s.result != null && body(s.result));
}

function bliss(r) {
  const peakDb = r.peak > 0 ? `${(20 * Math.log10(r.peak)).toFixed(1)} dBFS` : null;
  return h('div', { class: 'stack' },
    facts([
      ['Loudness', r.loudness != null ? `${r.loudness.toFixed(1)} LUFS` : null],
      ['True peak', peakDb],
    ]),
    h('details', {}, h('summary', {}, `Features (${r.features.length})`),
      bars(r.features.map((f) => [humanize(f.name), f.value]), { signed: true })),
    neighbours('Sounds closest', r.nearest, (n) => `distance ${n.score.toFixed(3)}${n.score < r.duplicate_distance ? ' · same recording?' : ''}`));
}

function rhythm(r) {
  const beat = r.bpm > 0;
  return h('div', { class: 'stack' },
    h('dl', { class: 'stats' },
      stat('BPM', beat ? r.bpm.toFixed(1) : '—'),
      stat('Beat', `${Math.round(r.beat_confidence * 100)}%`),
      stat('Key', r.key),
      stat('Key sure', `${Math.round(r.key_confidence * 100)}%`)),
    facts([
      ['Key', r.key_name],
      ['Beats', beat ? `${r.beats.toLocaleString()} beats, ${r.bars.toLocaleString()} bars of 4` : 'no steady beat'],
      ['First downbeat', r.first_downbeat != null ? `${r.first_downbeat.toFixed(2)} s` : null],
    ]),
    r.energy.length > 1 && h('figure', { class: 'energy' },
      sparkline(r.energy),
      h('figcaption', { class: 'muted small' },
        `Energy per ${beat ? 'bar' : `${r.energy_step ?? 2} seconds`}: ${Math.min(...r.energy).toFixed(0)} to ${Math.max(...r.energy).toFixed(0)} dB`)));
}

function chromaprint(r) {
  return h('div', { class: 'stack' },
    facts([
      ['Covers', r.duration != null ? `${fmtTime(r.duration)} of audio` : null],
      ['Size', `${r.items.toLocaleString()} items`],
    ]),
    r.duplicates.length
      ? h('div', {}, h('h3', { class: 'small-head' }, 'The same recording elsewhere?'),
        h('ul', { class: 'plain-list' }, r.duplicates.map((d) => h('li', {}, trackLink(d),
          h('span', { class: 'muted small' }, d.same_recording ? ' — fingerprints match' : ' — sounds alike, fingerprints differ')))))
      : h('p', { class: 'muted small' }, 'No other copy of this recording in the library.'));
}

function effnet(r) {
  const l = r.labels;
  const group = (title, items, max) => items.length > 0 && h('div', {},
    h('h3', { class: 'small-head' }, title),
    bars(items.slice(0, max).map((x) => [x.name.replace('---', ' › '), x.score])));
  return h('div', { class: 'stack' },
    group('Styles', l.styles, 8),
    group('Moods and qualities', l.moods, 12),
    group('Themes', l.themes, 5),
    group('Instruments', l.instruments, 5),
    neighbours('Closest in kind', r.nearest, (n) => `similarity ${n.score.toFixed(2)}`));
}

function musicbrainz(r) {
  const link = (kind, id) => h('a', { href: `https://musicbrainz.org/${kind}/${enc(id)}`, target: '_blank', rel: 'noopener' }, id);
  const how = { tag: 'from its tags', isrc: 'by its ISRC', acoustid: 'by its sound (AcoustID)', search: 'by artist, title and length', none: 'not found' };
  return facts([
    ['Found', how[r.how] || r.how],
    ['Recording', r.mbid ? link('recording', r.mbid) : null],
    ['Artist', r.artist_mbid ? link('artist', r.artist_mbid) : null],
    ['ISRC', r.isrc],
    ['Genres', r.genres.length ? r.genres.join(', ') : null],
  ]);
}

// ---------------------------------------------------------------- pieces

function trackLink(t) {
  return h('a', { href: `#/library/track/${enc(t.id)}?name=${enc(t.title || '')}` },
    t.title || `Track ${t.id}`, t.artist ? h('span', { class: 'muted' }, ` · ${t.artist}`) : null);
}

function neighbours(title, list, detail) {
  if (!list.length) return null;
  return h('div', {}, h('h3', { class: 'small-head' }, title),
    h('ol', { class: 'plain-list neighbours' }, list.map((n) => h('li', {}, trackLink(n),
      h('span', { class: 'muted small' }, ` — ${detail(n)}`)))));
}

/** Horizontal bars for [label, value] pairs: 0..1, or -1..1 around a middle line. */
function bars(pairs, { signed = false } = {}) {
  return h('div', { class: `bars${signed ? ' signed' : ''}`, role: 'list' }, pairs.map(([label, value]) => {
    const v = Math.max(signed ? -1 : 0, Math.min(1, value));
    const style = signed
      ? `left:${50 + Math.min(0, v) * 50}%;width:${Math.abs(v) * 50}%`
      : `width:${v * 100}%`;
    return h('div', { class: 'bar-row', role: 'listitem' },
      h('span', { class: 'bar-label' }, label),
      h('span', { class: 'bar-track' }, h('span', { class: 'bar-fill', style })),
      h('span', { class: 'bar-value' }, value.toFixed(signed ? 2 : 2)));
  }));
}

const SVG = 'http://www.w3.org/2000/svg';

/** The energy envelope as a filled line. */
function sparkline(values) {
  const w = 600, ht = 80;
  const lo = Math.min(...values), hi = Math.max(...values);
  const span = hi - lo || 1;
  const x = (i) => (i / (values.length - 1)) * w;
  const y = (v) => ht - 2 - ((v - lo) / span) * (ht - 4);
  const line = values.map((v, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)},${y(v).toFixed(1)}`).join('');
  const svg = document.createElementNS(SVG, 'svg');
  svg.setAttribute('viewBox', `0 0 ${w} ${ht}`);
  svg.setAttribute('preserveAspectRatio', 'none');
  svg.setAttribute('class', 'spark');
  svg.setAttribute('role', 'img');
  svg.setAttribute('aria-label', 'Energy over the song');
  const area = document.createElementNS(SVG, 'path');
  area.setAttribute('d', `${line}L${w},${ht}L0,${ht}Z`);
  area.setAttribute('class', 'spark-area');
  const path = document.createElementNS(SVG, 'path');
  path.setAttribute('d', line);
  path.setAttribute('class', 'spark-line');
  svg.append(area, path);
  return svg;
}

function stat(label, value) {
  return h('div', {}, h('dt', {}, label), h('dd', {}, value));
}

function humanize(name) {
  return String(name).replace(/_/g, ' ').replace(/\bstd\b/, 'deviation').replace(/^\w/, (c) => c.toUpperCase());
}
