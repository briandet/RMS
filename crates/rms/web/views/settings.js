// Settings: appearance, player settings & sync, library (name, folders, rescan), server info.

import { rpc, serverRoot } from '../api.js';
import { store, on, currentPlayer } from '../store.js';
import { h, icon, toast, plural } from '../util.js';
import { connectHelp } from './common.js';

export const cacheable = false;

const TRANSITIONS = [[0, 'None'], [1, 'Crossfade'], [2, 'Fade in'], [3, 'Fade out'], [4, 'Fade in and out']];
const REPLAY_GAIN = [[0, 'Off'], [1, 'Track gain'], [2, 'Album gain'], [3, 'Smart gain (album when playing whole album)']];
const BITRATES = [[0, 'No limit'], [320, '320 kbps'], [256, '256 kbps'], [192, '192 kbps'], [160, '160 kbps'], [128, '128 kbps'], [96, '96 kbps'], [64, '64 kbps']];

let uid = 0;
const fieldId = () => `f${++uid}`;

export function render(node, { ctx }) {
  document.title = 'Settings · RMS';
  const offs = [];
  let alive = true;

  const playerSection = h('section', { class: 'settings-card', 'aria-labelledby': 'set-player' });
  const syncSection = h('section', { class: 'settings-card', 'aria-labelledby': 'set-sync' });
  const libSection = h('section', { class: 'settings-card', 'aria-labelledby': 'set-library' });
  const scanSection = h('section', { class: 'settings-card', 'aria-labelledby': 'set-scan' });
  const aboutSection = h('section', { class: 'settings-card', 'aria-labelledby': 'set-about' });

  node.append(
    h('h1', { class: 'view-title' }, 'Settings'),
    h('div', { class: 'settings-grid' }, playerSection, syncSection, libSection, scanSection, appearance(ctx), aboutSection));

  // ---------------------------------------------------------------- player
  let renderedName = null;
  async function renderPlayer() {
    const pid = store.playerId;
    const p = currentPlayer();
    if (!pid || !p) {
      playerSection.replaceChildren(h('h2', { id: 'set-player' }, 'Player'), store.players.length ? h('p', { class: 'muted' }, 'Select a player in the header.') : connectHelp());
      return;
    }
    renderedName = p.name;
    playerSection.replaceChildren(h('h2', { id: 'set-player' }, `Player: ${p.name}`), h('div', { class: 'loading' }, h('span', { class: 'spinner' })));
    const names = ['transitionType', 'transitionDuration', 'replayGainMode', 'maxBitrate', 'digitalVolumeControl'];
    const vals = await Promise.allSettled(names.map((n) => rpc(['playerpref', n, '?'], pid)));
    if (!alive || pid !== store.playerId) return;
    const pref = {};
    names.forEach((n, i) => { pref[n] = vals[i].status === 'fulfilled' ? vals[i].value._p2 : undefined; });

    const setPref = async (name, value) => {
      const r = await ctx.cmd('playerpref', name, value);
      if (r) toast('Setting saved');
    };

    // Name
    const nameId = fieldId();
    const nameInput = h('input', { id: nameId, type: 'text', value: p.name || '', maxlength: 64, required: true });
    const nameForm = h('form', { class: 'field inline', onsubmit: async (e) => {
      e.preventDefault();
      const v = nameInput.value.trim();
      if (!v || v === p.name) return;
      const r = await ctx.cmd('name', v);
      if (r) { toast(`Player renamed to “${v}”`); ctx.refreshServer(); }
    } },
    h('label', { for: nameId }, 'Name'),
    h('div', { class: 'inline-row' }, nameInput, h('button', { class: 'btn', type: 'submit' }, 'Rename')));

    // Transition
    const durId = fieldId();
    const durOut = h('output', { for: durId }, `${pref.transitionDuration ?? 0} s`);
    const dur = h('input', { id: durId, type: 'range', class: 'range', min: 0, max: 10, step: 1, value: pref.transitionDuration ?? 0,
      disabled: pref.transitionDuration === undefined,
      oninput: (e) => { durOut.textContent = `${e.target.value} s`; },
      onchange: (e) => setPref('transitionDuration', +e.target.value) });

    const digitalId = fieldId();
    playerSection.replaceChildren(
      h('h2', { id: 'set-player' }, `Player: ${p.name}`),
      h('p', { class: 'muted small' }, [p.modelname || p.model, p.ip, p.playerid].filter(Boolean).join(' · ')),
      nameForm,
      selectField('Transition', TRANSITIONS, pref.transitionType, (v) => setPref('transitionType', v)),
      h('div', { class: 'field' }, h('label', { for: durId }, 'Transition duration'), h('div', { class: 'inline-row' }, dur, durOut)),
      selectField('Replay gain', REPLAY_GAIN, pref.replayGainMode, (v) => setPref('replayGainMode', v)),
      selectField('Maximum bitrate', BITRATES, pref.maxBitrate, (v) => setPref('maxBitrate', v), 'Higher bitrates are transcoded down when the player or network needs it.'),
      h('div', { class: 'field check' },
        h('input', { id: digitalId, type: 'checkbox', checked: +pref.digitalVolumeControl !== 0, disabled: pref.digitalVolumeControl === undefined,
          onchange: (e) => setPref('digitalVolumeControl', e.target.checked ? 1 : 0) }),
        h('label', { for: digitalId }, 'Digital volume control', h('span', { class: 'muted small block' }, 'Turn off to output fixed (100%) volume to an external amplifier.'))),
    );
  }

  // ---------------------------------------------------------------- sync
  let syncSig = '';
  function renderSync(force = false) {
    const pid = store.playerId;
    const st = store.status || {};
    const group = new Set([st.sync_master, ...String(st.sync_slaves || '').split(',')].filter(Boolean));
    group.delete(pid);
    const others = store.players.filter((p) => p.playerid !== pid);
    const sig = `${pid}|${[...group].join(',')}|${others.map((p) => p.playerid + p.name).join(',')}`;
    if (sig === syncSig && !force) return;
    syncSig = sig;
    const nameOf = (id) => store.players.find((p) => p.playerid === id)?.name || id;
    const head = h('h2', { id: 'set-sync' }, 'Synchronization');
    if (!pid) { syncSection.replaceChildren(head, h('p', { class: 'muted' }, 'No player selected.')); return; }
    if (!others.length) { syncSection.replaceChildren(head, h('p', { class: 'muted' }, 'Connect another player to play in sync across rooms.')); return; }

    const selId = fieldId();
    const candidates = others.filter((p) => !group.has(p.playerid));
    const sel = h('select', { id: selId }, candidates.map((p) => h('option', { value: p.playerid }, p.name || p.playerid)));
    syncSection.replaceChildren(...[head,
      group.size
        ? h('div', { class: 'sync-group' },
          h('p', {}, icon('link'), ' Synced with ', h('strong', {}, [...group].map(nameOf).join(', '))),
          h('button', { class: 'btn', type: 'button', onclick: async () => {
            if (await ctx.cmd('sync', '-')) { toast('Player unsynced'); ctx.refreshServer(); }
          } }, 'Unsync this player'))
        : h('p', { class: 'muted' }, 'This player is playing on its own.'),
      candidates.length ? h('form', { class: 'field inline', onsubmit: async (e) => {
        e.preventDefault();
        if (!sel.value) return;
        if (await ctx.cmd('sync', sel.value)) { toast(`Synced with ${nameOf(sel.value)}`); ctx.refreshServer(); }
      } },
      h('label', { for: selId }, 'Sync with another player'),
      h('div', { class: 'inline-row' }, sel, h('button', { class: 'btn', type: 'submit' }, icon('link'), 'Sync'))) : null].filter(Boolean));
  }

  // ---------------------------------------------------------------- library
  async function renderLibrary() {
    libSection.replaceChildren(h('h2', { id: 'set-library' }, 'Library'), h('div', { class: 'loading' }, h('span', { class: 'spinner' })));
    const [nameRes, dirsRes] = await Promise.allSettled([rpc(['pref', 'libraryname', '?']), rpc(['pref', 'mediadirs', '?'])]);
    if (!alive) return;
    const libName = nameRes.status === 'fulfilled' ? nameRes.value._p2 ?? '' : '';
    let dirs = dirsRes.status === 'fulfilled' ? dirsRes.value._p2 : [];
    if (typeof dirs === 'string') dirs = dirs ? dirs.split(/[,;\n]/).map((s) => s.trim()).filter(Boolean) : [];
    if (!Array.isArray(dirs)) dirs = [];

    const nameId = fieldId();
    const nameInput = h('input', { id: nameId, type: 'text', value: libName, placeholder: 'My Music' });
    const nameForm = h('form', { class: 'field inline', onsubmit: async (e) => {
      e.preventDefault();
      const r = await ctx.serverCmd('pref', 'libraryname', nameInput.value.trim());
      if (r) toast('Library name saved');
    } }, h('label', { for: nameId }, 'Library name'), h('div', { class: 'inline-row' }, nameInput, h('button', { class: 'btn', type: 'submit' }, 'Save')));

    // Music folders editor
    const dirList = h('ul', { class: 'dir-list', 'aria-label': 'Music folders' });
    const addDir = (path = '') => {
      const input = h('input', { type: 'text', value: path, placeholder: '/path/to/music', 'aria-label': 'Music folder path', spellcheck: 'false' });
      const li = h('li', {}, icon('folder'), input,
        h('button', { class: 'icon-btn', type: 'button', 'aria-label': `Remove folder ${path || ''}`.trim(), title: 'Remove', onclick: () => { li.remove(); } }, icon('close')));
      dirList.append(li);
      return input;
    };
    dirs.forEach((d) => addDir(d));
    if (!dirs.length) addDir();
    const dirsForm = h('form', { class: 'field', onsubmit: async (e) => {
      e.preventDefault();
      const values = [...dirList.querySelectorAll('input')].map((i) => i.value.trim()).filter(Boolean);
      const unique = [...new Set(values)];
      const r = await ctx.serverCmd('pref', 'mediadirs', unique);
      if (r) toast(`Saved ${plural(unique.length, 'music folder')}. Rescan to pick up changes.`);
    } },
    h('label', {}, 'Music folders'),
    h('p', { class: 'muted small' }, 'Folders on the server that contain your music.'),
    dirList,
    h('div', { class: 'inline-row' },
      h('button', { class: 'btn btn-ghost', type: 'button', onclick: () => addDir().focus() }, icon('add'), 'Add folder'),
      h('button', { class: 'btn btn-primary', type: 'submit' }, 'Save folders')));

    libSection.replaceChildren(h('h2', { id: 'set-library' }, 'Library'), nameForm, dirsForm);
  }

  // ---------------------------------------------------------------- scan
  const scanStatus = h('div', { class: 'scan-status', role: 'status', 'aria-live': 'polite' });
  const scanBtn = (label, args, confirmMsg, primary) => h('button', { class: `btn${primary ? ' btn-primary' : ''}`, type: 'button', onclick: async () => {
    if (confirmMsg && !confirm(confirmMsg)) return;
    const r = await ctx.serverCmd(...args);
    if (r) { toast(`${label} started`); ctx.fastServerPoll(20000); }
  } }, label);
  scanSection.append(
    h('h2', { id: 'set-scan' }, 'Scan'),
    scanStatus,
    h('div', { class: 'button-row' },
      scanBtn('Rescan library', ['rescan'], null, true),
      scanBtn('Rescan playlists', ['rescan', 'playlists']),
      scanBtn('Clear library and rescan', ['wipecache'], 'This deletes the library database and rebuilds it from scratch. Continue?')));

  function renderScan() {
    const s = store.server || {};
    const scanning = +s.rescan > 0;
    const done = +s.progressdone || 0, total = +s.progresstotal || 0;
    const stats = h('dl', { class: 'stats' },
      stat('Albums', s['info total albums']), stat('Artists', s['info total artists']),
      stat('Tracks', s['info total songs']), stat('Genres', s['info total genres']));
    if (scanning) {
      const pct = total ? Math.round((done / total) * 100) : null;
      scanStatus.replaceChildren(
        h('p', {}, h('span', { class: 'spinner inline' }), ' Scanning', s.progressname ? `: ${humanize(s.progressname)}` : '…'),
        h('progress', { max: total || 1, value: total ? done : null, 'aria-label': 'Scan progress' }),
        h('p', { class: 'muted small' }, total ? `${done.toLocaleString()} of ${total.toLocaleString()} (${pct}%)` : 'Preparing…'),
        stats);
    } else {
      const last = +s.lastscan ? new Date(+s.lastscan * 1000).toLocaleString() : 'never';
      scanStatus.replaceChildren(h('p', {}, 'Last scan: ', h('strong', {}, last)), stats);
    }
    aboutSection.replaceChildren(
      h('h2', { id: 'set-about' }, 'About'),
      h('dl', { class: 'about' },
        h('dt', {}, 'Server version'), h('dd', {}, s.version || '—'),
        h('dt', {}, 'Server address'), h('dd', {}, h('code', {}, new URL(serverRoot, location.href).href)),
        h('dt', {}, 'Players'), h('dd', {}, String(store.players.length))),
      h('p', { class: 'muted small' }, 'Keyboard: Space = play/pause, / = search.'));
  }

  // ---------------------------------------------------------------- wire up
  renderPlayer();
  renderSync(true);
  renderLibrary();
  renderScan();
  offs.push(
    on('player', () => { renderPlayer(); renderSync(true); }),
    on('players', () => { renderSync(); if (currentPlayer()?.name !== renderedName) renderPlayer(); }),
    on('status', () => renderSync()),
    on('server', renderScan),
  );
  ctx.refreshServer();

  return () => { alive = false; offs.forEach((f) => f()); };
}

function appearance(ctx) {
  const id = fieldId();
  const sel = h('select', { id, onchange: (e) => ctx.setTheme(e.target.value) },
    [['auto', 'Match system'], ['light', 'Light'], ['dark', 'Dark']].map(([v, l]) => h('option', { value: v, selected: ctx.getTheme() === v }, l)));
  return h('section', { class: 'settings-card', 'aria-labelledby': 'set-look' },
    h('h2', { id: 'set-look' }, 'Appearance'),
    h('div', { class: 'field' }, h('label', { for: id }, 'Theme'), sel));
}

function selectField(label, options, value, onChange, help) {
  const id = fieldId();
  const known = options.some(([v]) => String(v) === String(value));
  const opts = known || value === undefined ? options : [...options, [value, String(value)]];
  return h('div', { class: 'field' },
    h('label', { for: id }, label),
    h('select', { id, disabled: value === undefined, onchange: (e) => onChange(+e.target.value) },
      opts.map(([v, l]) => h('option', { value: v, selected: String(v) === String(value ?? '') }, l))),
    value === undefined && h('p', { class: 'muted small' }, 'Not available for this player.'),
    help && h('p', { class: 'muted small' }, help));
}

function stat(label, value) {
  return h('div', {}, h('dt', {}, label), h('dd', {}, value != null ? (+value).toLocaleString() : '—'));
}

function humanize(name) {
  return String(name).replace(/_/g, ' ').replace(/\b(\w)/, (c) => c.toUpperCase());
}
