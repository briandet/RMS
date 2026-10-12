// Settings: appearance, player settings & sync, library (name, folders, rescan), server info.

import {
  serverRoot, getPlayerSettings, patchPlayerSettings, getSettings, patchSettings, scanLibrary,
  getSpotify, spotifyLogin, spotifyLogout, patchSpotify, forgetSpotifyLearned,
  addListenBrainzAccount, patchListenBrainzAccount, removeListenBrainzAccount,
} from '../api.js';
import { store, on, currentPlayer } from '../store.js';
import { h, icon, toast, plural } from '../util.js';
import { connectHelp } from './common.js';
import { browserPlayer, setBrowserPlayer, playableFormats, webAudioSupported } from '../browser-player.js';

export const cacheable = false;

const TRANSITIONS = [[0, 'None'], [1, 'Crossfade'], [2, 'Fade in'], [3, 'Fade out'], [4, 'Fade in and out']];
const REPLAY_GAIN = [[0, 'Off'], [1, 'Track gain'], [2, 'Album gain'], [3, 'Smart gain (album when playing whole album)']];
const ADVENTUROUS = [[0, 'Off'], [5, 'A little (5%)'], [10, '10%'], [15, 'Some (15%)'], [25, '25%'], [40, 'A lot (40%)']];
const BITRATES = [[0, 'No limit'], [320, '320 kbps'], [256, '256 kbps'], [192, '192 kbps'], [160, '160 kbps'], [128, '128 kbps'], [96, '96 kbps'], [64, '64 kbps']];

let uid = 0;
const fieldId = () => `f${++uid}`;

export function render(node, { ctx }) {
  document.title = 'Settings · RMS';
  const offs = [];
  let alive = true;

  const playerSection = h('section', { class: 'settings-card', 'aria-labelledby': 'set-player' });
  const displaySection = h('section', { class: 'settings-card', 'aria-labelledby': 'set-display', hidden: true });
  const syncSection = h('section', { class: 'settings-card', 'aria-labelledby': 'set-sync' });
  const browserSection = h('section', { class: 'settings-card', 'aria-labelledby': 'set-browser' });
  const secureSection = h('section', { class: 'settings-card', 'aria-labelledby': 'set-secure' });
  const devicesSection = h('section', { class: 'settings-card', 'aria-labelledby': 'set-devices' });
  const spotifySection = h('section', { class: 'settings-card', 'aria-labelledby': 'set-spotify' });
  const libSection = h('section', { class: 'settings-card', 'aria-labelledby': 'set-library' });
  const lbSection = h('section', { class: 'settings-card', 'aria-labelledby': 'set-listenbrainz' });
  const scanSection = h('section', { class: 'settings-card', 'aria-labelledby': 'set-scan' });
  const aboutSection = h('section', { class: 'settings-card', 'aria-labelledby': 'set-about' });

  node.append(
    h('h1', { class: 'view-title' }, 'Settings'),
    h('div', { class: 'settings-grid' }, playerSection, displaySection, syncSection, browserSection, secureSection, devicesSection, spotifySection, lbSection, libSection, scanSection, appearance(ctx), aboutSection));

  // Server settings: PATCH /settings, toast the outcome; resolves to the new settings or null.
  async function saveServer(patch, done = 'Setting saved') {
    try {
      const r = await patchSettings(patch);
      if (done) toast(done);
      return r;
    } catch (e) {
      toast(`Could not save: ${e.message}`, 'error');
      return null;
    }
  }

  // ---------------------------------------------------------------- player
  let renderedName = null;
  let playerSeq = 0;
  async function renderPlayer() {
    const pid = store.playerId;
    const p = currentPlayer();
    const seq = ++playerSeq;
    if (!pid || !p) {
      playerSection.replaceChildren(h('h2', { id: 'set-player' }, 'Player'), store.players.length ? h('p', { class: 'muted' }, 'Select a player in the header.') : connectHelp());
      renderDisplay(null);
      return;
    }
    renderedName = p.name;
    playerSection.replaceChildren(h('h2', { id: 'set-player' }, `Player: ${p.name}`), h('div', { class: 'loading' }, h('span', { class: 'spinner' })));
    let ps = null, err = null;
    try { ps = await getPlayerSettings(pid); } catch (e) { err = e; }
    if (!alive || pid !== store.playerId || seq !== playerSeq) return;
    const pref = ps || {};

    const setPref = async (patch) => {
      try {
        const r = await patchPlayerSettings(pid, patch);
        toast('Setting saved');
        return r;
      } catch (e) {
        toast(`Could not save: ${e.message}`, 'error');
        return null;
      }
    };

    // Name
    const nameId = fieldId();
    const nameInput = h('input', { id: nameId, type: 'text', value: p.name || '', maxlength: 64, required: true });
    const nameForm = h('form', { class: 'field inline', onsubmit: async (e) => {
      e.preventDefault();
      const v = nameInput.value.trim();
      if (!v || v === p.name) return;
      const r = await ctx.send({ command: 'rename', name: v });
      if (r) toast(`Player renamed to “${v}”`);
    } },
    h('label', { for: nameId }, 'Name'),
    h('div', { class: 'inline-row' }, nameInput, h('button', { class: 'btn', type: 'submit' }, 'Rename')));

    // Transition
    const durId = fieldId();
    const durOut = h('output', { for: durId }, `${pref.transition_duration ?? 0} s`);
    const dur = h('input', { id: durId, type: 'range', class: 'range', min: 0, max: 10, step: 1, value: pref.transition_duration ?? 0,
      disabled: pref.transition_duration === undefined,
      oninput: (e) => { durOut.textContent = `${e.target.value} s`; },
      onchange: (e) => setPref({ transition_duration: +e.target.value }) });

    // Don't Stop The Music: the server lists the choices with their labels
    const dstmField = pref.dont_stop_the_music?.options?.length
      ? choiceField('Don’t Stop The Music', pref.dont_stop_the_music, (v) => setPref({ dont_stop_the_music: v }),
        'When the queue runs out, keep playing: songs similar to the last ones, a random mix, or one of your favorites.')
      : null;

    const digitalId = fieldId();
    playerSection.replaceChildren(...[
      h('h2', { id: 'set-player' }, `Player: ${p.name}`),
      h('p', { class: 'muted small' }, [p.model_name || p.model, p.ip, p.id].filter(Boolean).join(' · ')),
      err && h('p', { class: 'muted small', role: 'alert' }, `Could not load the player’s settings: ${err.message}`),
      nameForm,
      selectField('Transition', TRANSITIONS, pref.transition_type, (v) => setPref({ transition_type: v })),
      h('div', { class: 'field' }, h('label', { for: durId }, 'Transition duration'), h('div', { class: 'inline-row' }, dur, durOut)),
      dstmField,
      scrobbleField(pref, setPref),
      pref.adventurous !== undefined && selectField('Adventurous', ADVENTUROUS, pref.adventurous, (v) => setPref({ adventurous: v }),
        'How much of Similar Songs and Create Mix is music you haven’t played: songs ListenBrainz finds similar, from your library or Spotify. Needs “Discover music with ListenBrainz” under Library.'),
      selectField('Replay gain', REPLAY_GAIN, pref.replay_gain_mode, (v) => setPref({ replay_gain_mode: v })),
      selectField('Maximum bitrate', BITRATES, pref.max_bitrate, (v) => setPref({ max_bitrate: v }), 'Higher bitrates are transcoded down when the player or network needs it.'),
      h('div', { class: 'field check' },
        h('input', { id: digitalId, type: 'checkbox', checked: !!pref.digital_volume_control, disabled: pref.digital_volume_control === undefined,
          onchange: (e) => setPref({ digital_volume_control: e.target.checked }) }),
        h('label', { for: digitalId }, 'Digital volume control', h('span', { class: 'muted small block' }, 'Turn off to output fixed (100%) volume to an external amplifier.'))),
    ].filter(Boolean));
    renderDisplay(ps?.display ? { display: ps.display, setPref } : null);
  }

  // ---------------------------------------------------------------- display (Squeezebox screens)
  // Present only for players with a screen; the server sends the labels of every choice.
  function renderDisplay(state) {
    displaySection.hidden = !state;
    if (!state) { displaySection.replaceChildren(); return; }
    const { display: d, setPref } = state;
    const choice = (label, key) => d[key]?.options?.length ? choiceField(label, d[key], (v) => setPref({ [key]: v })) : null;
    const slider = (label, key) => {
      const r = d[key];
      if (!r) return null;
      const id = fieldId();
      const out = h('output', { for: id }, String(r.value));
      return h('div', { class: 'field' }, h('label', { for: id }, label),
        h('div', { class: 'inline-row' },
          h('input', { id, type: 'range', class: 'range', min: r.min, max: r.max, step: 1, value: r.value,
            oninput: (e) => { out.textContent = e.target.value; }, onchange: (e) => setPref({ [key]: +e.target.value }) }), out));
    };
    displaySection.replaceChildren(...[
      h('h2', { id: 'set-display' }, 'Player display'),
      h('p', { class: 'muted small' }, 'The Now Playing button on the remote cycles the playing display; Size and Brightness change the text size and brightness.'),
      choice('Now Playing information', 'playing_mode'),
      choice('Text size', 'text_size'),
      choice('Text size when off', 'text_size_off'),
      choice('Brightness when on', 'brightness_on'),
      choice('Brightness when off', 'brightness_off'),
      choice('Brightness when idle', 'brightness_idle'),
      slider('Automatic brightness: minimum', 'auto_brightness_min'),
      slider('Automatic brightness: sensitivity', 'auto_brightness_sensitivity'),
    ].filter(Boolean));
  }

  // ---------------------------------------------------------------- spotify
  let spotifyPoll = null;
  // The user's own Spotify app: the shared default client id is rate-limited by Spotify.
  function spotifyClientIdField(settings) {
    const id = fieldId();
    const input = h('input', { id, type: 'text', value: settings?.spotify_client_id ?? '', placeholder: '32 hex digits', spellcheck: 'false', autocomplete: 'off' });
    return h('form', { class: 'field', onsubmit: async (e) => {
      e.preventDefault();
      const v = input.value.trim();
      if (v && !/^[0-9a-fA-F]{32}$/.test(v)) { toast('A client ID is 32 hexadecimal digits', 'error'); return; }
      await saveServer({ spotify_client_id: v }, 'Saved: sign out and sign in again to use it');
    } },
    h('label', { for: id }, 'Client ID of your own Spotify app (recommended)'),
    h('p', { class: 'muted small' }, 'Spotify limits the shared client ID RMS uses by default, so browsing and track names may not load. Create an app at developer.spotify.com/dashboard with the redirect URI http://127.0.0.1:5588/login and the Web API, paste its client ID here, then sign in again.'),
    h('div', { class: 'inline-row' }, input, h('button', { class: 'btn', type: 'submit' }, 'Save')));
  }

  async function renderSpotify() {
    const head = h('h2', { id: 'set-spotify' }, 'Spotify');
    let st = null, err = null;
    const [stRes, setRes] = await Promise.allSettled([getSpotify(), getSettings()]);
    if (!alive) return;
    if (stRes.status === 'fulfilled') st = stRes.value;
    else if (stRes.reason?.status !== 404) err = stRes.reason;
    const settings = setRes.status === 'fulfilled' ? setRes.value : null;
    if (!st) {
      spotifySection.replaceChildren(head, h('p', { class: 'muted' }, err ? `Could not get the Spotify status: ${err.message}` : 'Spotify is not available.'));
      return;
    }
    if (st.signed_in) {
      clearInterval(spotifyPoll);
      const normId = fieldId();
      const learnId = fieldId();
      spotifySection.replaceChildren(...[head,
        h('p', {}, 'Signed in', st.user ? [' as ', h('strong', {}, st.user)] : '', st.product ? ` (${st.product})` : '', '.'),
        st.error && h('p', { class: 'muted small' }, st.error),
        st.product && st.product !== 'premium' && h('p', { class: 'muted small' }, 'Playback needs Spotify Premium.'),
        selectField('Streaming quality', [[96, 'Low (96 kbps)'], [160, 'Normal (160 kbps)'], [320, 'High (320 kbps)']],
          settings?.spotify_bitrate, (v) => saveServer({ spotify_bitrate: v })),
        h('div', { class: 'field check' },
          h('input', { id: normId, type: 'checkbox', checked: !!settings?.spotify_normalisation, disabled: !settings,
            onchange: (e) => saveServer({ spotify_normalisation: e.target.checked }) }),
          h('label', { for: normId }, 'Normalise volume', h('span', { class: 'muted small block' }, 'Spotify’s loudness normalisation, like its apps.'))),
        h('div', { class: 'field check' },
          h('input', { id: learnId, type: 'checkbox', checked: !!st.learn, onchange: async (e) => {
            try { await patchSpotify({ learn: e.target.checked }); } catch (err) { toast(`Could not save: ${err.message}`, 'error'); }
            renderSpotify();
          } }),
          h('label', { for: learnId }, 'Use what I play in mixes',
            h('span', { class: 'muted small block' },
              'Learns how each track you play to the end sounds (tempo, key, timbre — no audio is kept), so Create Mix and Don’t Stop The Music can pick Spotify tracks next to your own. ',
              `${st.learned} track${st.learned === 1 ? '' : 's'} learned.`))),
        st.learned ? h('div', { class: 'button-row' }, h('button', { class: 'btn btn-small', type: 'button', onclick: async () => {
          if (!confirm('Forget every learned Spotify track?')) return;
          try { await forgetSpotifyLearned(); toast('Forgotten'); } catch (err) { toast(`Could not forget: ${err.message}`, 'error'); }
          renderSpotify();
        } }, 'Forget learned tracks')) : null,
        h('p', {}, h('a', { href: '#/library/spotify' }, 'Browse Spotify'), ' · also in the player menus and Squeezer.'),
        spotifyClientIdField(settings),
        ...spotifySignIn('Sign in again (keeps playback working)', false),
        h('div', { class: 'button-row' }, h('button', { class: 'btn', type: 'button', onclick: async () => {
          if (!confirm('Sign out of Spotify on this server?')) return;
          try { await spotifyLogout(); } catch (e) { toast(`Could not sign out: ${e.message}`, 'error'); }
          renderSpotify();
        } }, 'Sign out'))].filter(Boolean));
      return;
    }
    spotifySection.replaceChildren(...[head,
      h('p', {}, 'Play Spotify on your players and browse it from the player menus, Squeezer and here. Needs Spotify Premium.'),
      st.error && h('p', { class: 'muted small' }, st.error),
      ...spotifySignIn('Sign in with Spotify', true),
      spotifyClientIdField(settings)].filter(Boolean));
  }

  // Sign-in button plus the "paste the redirected address" form.  `poll`: also finish by
  // itself when the browser runs on the server (only meaningful while signed out).
  function spotifySignIn(label, poll) {
    const pasteId = fieldId();
    const paste = h('input', { id: pasteId, type: 'url', placeholder: 'http://127.0.0.1:5588/login?code=…', spellcheck: 'false' });
    const msg = h('p', { class: 'muted small', role: 'status', 'aria-live': 'polite' });
    const finish = h('form', { class: 'field', hidden: true, onsubmit: async (e) => {
      e.preventDefault();
      const v = paste.value.trim();
      if (!v) return;
      if (/^https?:\/\/accounts\.spotify\.com\//.test(v)) {
        msg.textContent = 'That is Spotify’s own sign-in page. Agree there first, then paste the address you end up on (it starts with http://127.0.0.1:5588/login).';
        return;
      }
      msg.textContent = 'Signing in…';
      const r = await spotifyLogin(v).catch((err) => ({ error: err.message }));
      if (!alive) return;
      if (r.user) { toast(`Signed in to Spotify as ${r.user}`); renderSpotify(); } else msg.textContent = r.error || 'Sign-in failed.';
    } },
    h('label', { for: pasteId }, 'Address of the page Spotify sent you to'),
    h('p', { class: 'muted small' }, 'After you agree, Spotify sends your browser to an address starting with http://127.0.0.1:5588/login. If this server runs on another computer that page won’t load — copy its full address from the address bar and paste it here.'),
    h('div', { class: 'inline-row' }, paste, h('button', { class: 'btn btn-primary', type: 'submit' }, 'Finish')),
    msg);
    const button = h('div', { class: 'button-row' }, h('button', { class: poll ? 'btn btn-primary' : 'btn', type: 'button', onclick: async () => {
      let r;
      try { r = await spotifyLogin(); } catch (e) { toast(`Could not start the Spotify sign-in: ${e.message}`, 'error'); return; }
      if (!r?.url) { toast('Could not start the Spotify sign-in', 'error'); return; }
      window.open(r.url, '_blank', 'noopener');
      finish.hidden = false;
      paste.focus();
      if (!poll) return;
      clearInterval(spotifyPoll);
      spotifyPoll = setInterval(async () => {
        const s2 = await getSpotify().catch(() => null);
        if (s2?.signed_in) { clearInterval(spotifyPoll); toast('Signed in to Spotify'); renderSpotify(); }
      }, 3000);
    } }, label));
    return [button, finish];
  }

  // ---------------------------------------------------------------- sync
  let syncSig = '';
  function renderSync(force = false) {
    const pid = store.playerId;
    const group = new Set(currentPlayer()?.synced_with || []);
    const others = store.players.filter((p) => p.id !== pid);
    const sig = `${pid}|${[...group].join(',')}|${others.map((p) => p.id + p.name).join(',')}`;
    if (sig === syncSig && !force) return;
    syncSig = sig;
    const nameOf = (id) => store.players.find((p) => p.id === id)?.name || id;
    const head = h('h2', { id: 'set-sync' }, 'Synchronization');
    if (!pid) { syncSection.replaceChildren(head, h('p', { class: 'muted' }, 'No player selected.')); return; }
    if (!others.length) { syncSection.replaceChildren(head, h('p', { class: 'muted' }, 'Connect another player to play in sync across rooms.')); return; }

    const selId = fieldId();
    const candidates = others.filter((p) => !group.has(p.id));
    const sel = h('select', { id: selId }, candidates.map((p) => h('option', { value: p.id }, p.name || p.id)));
    syncSection.replaceChildren(...[head,
      group.size
        ? h('div', { class: 'sync-group' },
          h('p', {}, icon('link'), ' Synced with ', h('strong', {}, [...group].map(nameOf).join(', '))),
          h('button', { class: 'btn', type: 'button', onclick: async () => {
            if (await ctx.send({ command: 'unsync' })) toast('Player unsynced');
          } }, 'Unsync this player'))
        : h('p', { class: 'muted' }, 'This player is playing on its own.'),
      candidates.length ? h('form', { class: 'field inline', onsubmit: async (e) => {
        e.preventDefault();
        if (!sel.value) return;
        if (await ctx.send({ command: 'sync', with: sel.value })) toast(`Synced with ${nameOf(sel.value)}`);
      } },
      h('label', { for: selId }, 'Sync with another player'),
      h('div', { class: 'inline-row' }, sel, h('button', { class: 'btn', type: 'submit' }, icon('link'), 'Sync'))) : null].filter(Boolean));
  }

  // ---------------------------------------------------------------- ListenBrainz
  // Which accounts a player scrobbles to (filled in once the server settings are loaded).
  let lbAccounts = [];
  function scrobbleField(pref, setPref) {
    if (!lbAccounts.length || !Array.isArray(pref.scrobble_to)) return null;
    const chosen = new Set(pref.scrobble_to);
    return h('fieldset', { class: 'field' },
      h('legend', {}, 'Scrobble to'),
      ...lbAccounts.map((a) => {
        const id = fieldId();
        return h('div', { class: 'check' },
          h('input', { id, type: 'checkbox', checked: chosen.has(a.user), onchange: (e) => {
            if (e.target.checked) chosen.add(a.user); else chosen.delete(a.user);
            setPref({ scrobble_to: [...chosen] });
          } }),
          h('label', { for: id }, `${a.user} (ListenBrainz)`));
      }),
      h('p', { class: 'muted small' }, 'What this player (and the players synced with it) plays is sent to these ListenBrainz accounts.'));
  }

  async function renderListenBrainz() {
    const head = h('h2', { id: 'set-listenbrainz' }, 'ListenBrainz');
    let settings;
    try { settings = await getSettings(); } catch (e) {
      if (alive) lbSection.replaceChildren(head, h('p', { class: 'muted', role: 'alert' }, `Could not load the settings: ${e.message}`));
      return;
    }
    if (!alive) return;
    const accounts = settings.listenbrainz_accounts || [];
    const changed = accounts.length !== lbAccounts.length;
    lbAccounts = accounts;
    if (changed) renderPlayer();
    const rows = accounts.map((a) => {
      const id = fieldId();
      return h('li', { class: 'inline-row' },
        h('strong', {}, a.user),
        h('span', { class: 'check' },
          h('input', { id, type: 'checkbox', checked: a.default, onchange: async (e) => {
            try { await patchListenBrainzAccount(a.user, { default: e.target.checked }); toast('Saved'); renderPlayer(); } catch (err) { toast(`Could not save: ${err.message}`, 'error'); }
          } }),
          h('label', { for: id }, 'Default')),
        h('button', { class: 'btn btn-ghost', type: 'button', onclick: async () => {
          if (!confirm(`Remove the ListenBrainz account ${a.user}? Listens not sent yet are dropped.`)) return;
          try { await removeListenBrainzAccount(a.user); toast(`Removed ${a.user}`); renderListenBrainz(); } catch (err) { toast(`Could not remove: ${err.message}`, 'error'); }
        } }, 'Remove'));
    });
    const tokenId = fieldId();
    const token = h('input', { id: tokenId, type: 'password', placeholder: 'User token', autocomplete: 'off', spellcheck: 'false' });
    const add = h('form', { class: 'field inline', onsubmit: async (e) => {
      e.preventDefault();
      if (!token.value.trim()) return;
      try {
        const a = await addListenBrainzAccount(token.value.trim());
        toast(`Added ${a.user}`);
        renderListenBrainz();
      } catch (err) { toast(`Could not add the account: ${err.message}`, 'error'); }
    } }, h('label', { for: tokenId }, accounts.length ? 'Add another account' : 'Add an account'),
    h('p', { class: 'muted small' }, 'Paste the user token from ',
      h('a', { href: 'https://listenbrainz.org/settings/', target: '_blank', rel: 'noopener' }, 'your ListenBrainz settings'), '.'),
    h('div', { class: 'inline-row' }, token, h('button', { class: 'btn', type: 'submit' }, 'Add')));
    const spId = fieldId();
    const status = settings.scrobble_pending
      ? h('p', { class: 'muted small', role: settings.scrobble_error ? 'alert' : null },
        `${plural(settings.scrobble_pending, 'listen')} waiting to be sent${settings.scrobble_error ? ` (${settings.scrobble_error})` : ''}.`)
      : null;
    lbSection.replaceChildren(...[head,
      h('p', { class: 'muted small' }, 'What plays here is sent to ListenBrainz as you listen (a song counts once it has played for half its length or 4 minutes). Its recommendations, and the playlists it makes for you, are under Library → ListenBrainz and, with “Discover music with ListenBrainz” on, find their way into mixes.'),
      accounts.length ? h('ul', { class: 'plain-list', 'aria-label': 'ListenBrainz accounts' }, ...rows) : null,
      accounts.length ? h('p', { class: 'muted small' }, 'Players scrobble to the default accounts unless they have their own choice (Player settings), and so do Subsonic apps.') : null,
      add,
      accounts.length ? h('div', { class: 'field check' },
        h('input', { id: spId, type: 'checkbox', checked: !!settings.scrobble_spotify, onchange: (e) => saveServer({ scrobble_spotify: e.target.checked }) }),
        h('label', { for: spId }, 'Scrobble Spotify plays too',
          h('span', { class: 'muted small block' }, 'Leave off if ListenBrainz already imports your listens from Spotify (Connect services on listenbrainz.org), or they count twice. Spotify Connect plays are never scrobbled.'))) : null,
      status,
    ].filter(Boolean));
  }

  // ---------------------------------------------------------------- library
  async function renderLibrary() {
    libSection.replaceChildren(h('h2', { id: 'set-library' }, 'Library'), h('div', { class: 'loading' }, h('span', { class: 'spinner' })));
    let settings;
    try { settings = await getSettings(); } catch (e) {
      if (alive) libSection.replaceChildren(h('h2', { id: 'set-library' }, 'Library'), h('p', { class: 'muted', role: 'alert' }, `Could not load the library settings: ${e.message}`));
      return;
    }
    if (!alive) return;
    const libName = settings.library_name ?? '';
    const artFolder = settings.art_folder ?? '';
    const onlineArt = !!settings.online_artist_images;
    const watching = !!settings.watch_folders;
    const dirs = Array.isArray(settings.media_dirs) ? settings.media_dirs : [];

    const nameId = fieldId();
    const nameInput = h('input', { id: nameId, type: 'text', value: libName, placeholder: 'My Music' });
    const nameForm = h('form', { class: 'field inline', onsubmit: async (e) => {
      e.preventDefault();
      await saveServer({ library_name: nameInput.value.trim() }, 'Library name saved');
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
      await saveServer({ media_dirs: unique }, `Saved ${plural(unique.length, 'music folder')}. Rescan to pick up changes.`);
    } },
    h('label', {}, 'Music folders'),
    h('p', { class: 'muted small' }, 'Folders on the server that contain your music.'),
    dirList,
    h('div', { class: 'inline-row' },
      h('button', { class: 'btn btn-ghost', type: 'button', onclick: () => addDir().focus() }, icon('add'), 'Add folder'),
      h('button', { class: 'btn btn-primary', type: 'submit' }, 'Save folders')));

    const watchId = fieldId();
    const watchField = h('div', { class: 'field check' },
      h('input', { id: watchId, type: 'checkbox', checked: watching, onchange: async (e) => {
        await saveServer({ watch_folders: e.target.checked },
          e.target.checked ? 'Changes in the music folders will be scanned as they happen' : 'Music folders are no longer watched');
      } }),
      h('label', { for: watchId }, 'Watch the music folders', h('span', { class: 'muted small block' }, 'Music added, changed or removed shows up by itself. Changes made on another computer to a network share may not be noticed: rescan then.')));

    // Artist pictures: <artfolder>/<Artist Name>.jpg, artist.jpg next to albums, optional download
    const artId = fieldId();
    const artInput = h('input', { id: artId, type: 'text', value: artFolder, placeholder: '/path/to/artist/pictures', spellcheck: 'false' });
    const artForm = h('form', { class: 'field inline', onsubmit: async (e) => {
      e.preventDefault();
      await saveServer({ art_folder: artInput.value.trim() }, 'Artist pictures folder saved. Rescan to pick up changes.');
    } }, h('label', { for: artId }, 'Artist pictures folder'),
    h('p', { class: 'muted small' }, 'Optional folder with pictures named after the artist (e.g. “Miles Davis.jpg”). An artist.jpg next to the album folders is found as well.'),
    h('div', { class: 'inline-row' }, artInput, h('button', { class: 'btn', type: 'submit' }, 'Save')));
    const onlineId = fieldId();
    const onlineField = h('div', { class: 'field check' },
      h('input', { id: onlineId, type: 'checkbox', checked: onlineArt, onchange: async (e) => {
        await saveServer({ online_artist_images: e.target.checked },
          e.target.checked ? 'Missing artist pictures will be downloaded' : 'Artist picture downloads off');
      } }),
      h('label', { for: onlineId }, 'Download missing artist pictures', h('span', { class: 'muted small block' }, 'Looks up artists without a local picture on Deezer after each scan.')));

    // MusicBrainz identification, and AcoustID for recognising recordings by their sound
    const mbId = fieldId();
    const keyId = fieldId();
    const mbOn = !!settings.musicbrainz;
    const progress = mbOn
      ? `${settings.musicbrainz_identified ?? 0} identified${settings.musicbrainz_pending ? `, ${settings.musicbrainz_pending} to look up` : ''}; `
        + `${settings.albums_matched ?? 0} albums matched to their release${settings.albums_uncertain ? `, ${settings.albums_uncertain} uncertain` : ''}${settings.albums_pending ? `, ${settings.albums_pending} to match` : ''}.`
      : '';
    const mbField = h('div', { class: 'field check' },
      h('input', { id: mbId, type: 'checkbox', checked: mbOn, onchange: async (e) => {
        await saveServer({ musicbrainz: e.target.checked },
          e.target.checked ? 'Recordings will be identified on MusicBrainz in the background' : 'MusicBrainz lookups off');
        renderLibrary();
      } }),
      h('label', { for: mbId }, 'Identify recordings with MusicBrainz',
        h('span', { class: 'muted small block' },
          'Finds each track on MusicBrainz (by its MusicBrainz id or ISRC tag, its sound, or its artist, title and length) and each album's release, and fills in what the tags leave out: genres for tracks without one, MusicBrainz ids and ISRCs, album years, track numbers and discs. Your files are never changed and tags always win. One lookup a second, so a large library takes some hours the first time. ',
          progress)));
    const keyInput = h('input', { id: keyId, type: 'text', value: settings.acoustid_key ?? '', placeholder: 'AcoustID application key', spellcheck: 'false', autocomplete: 'off' });
    const keyForm = h('form', { class: 'field inline', hidden: !mbOn, onsubmit: async (e) => {
      e.preventDefault();
      await saveServer({ acoustid_key: keyInput.value.trim() }, 'AcoustID key saved');
    } }, h('label', { for: keyId }, 'AcoustID key (optional)'),
    h('p', { class: 'muted small' }, 'Recognises recordings by their sound, for files with poor or no tags. Get a free key at ',
      h('a', { href: 'https://acoustid.org/new-application', target: '_blank', rel: 'noopener' }, 'acoustid.org'), '.'),
    h('div', { class: 'inline-row' }, keyInput, h('button', { class: 'btn', type: 'submit' }, 'Save')));

    const lbId = fieldId();
    const lbOn = !!settings.listenbrainz;
    const lbField = h('div', { class: 'field check' },
      h('input', { id: lbId, type: 'checkbox', checked: lbOn, onchange: async (e) => {
        await saveServer({ listenbrainz: e.target.checked },
          e.target.checked ? 'Mixes will include music you haven’t played' : 'Discovery off');
        renderLibrary();
      } }),
      h('label', { for: lbId }, 'Discover music with ListenBrainz',
        h('span', { class: 'muted small block' },
          'Mixes include some songs you haven’t played (each player’s “Adventurous” setting): played alongside the seeds by ListenBrainz listeners, or by artists they find similar. From your library when you have them, else from Spotify. Skipping one within 30 seconds rejects it. Needs MusicBrainz identification (above) or MusicBrainz ids in your tags. ',
          lbOn ? `${settings.discovered ?? 0} discovered, ${settings.discovered_played ?? 0} played, ${settings.discovered_rejected ?? 0} rejected.` : '')));

    libSection.replaceChildren(h('h2', { id: 'set-library' }, 'Library'), nameForm, dirsForm, watchField, artForm, onlineField, mbField, keyForm, lbField);
  }

  // ---------------------------------------------------------------- scan
  const scanStatus = h('div', { class: 'scan-status', role: 'status', 'aria-live': 'polite' });
  const scanBtn = (label, mode, confirmMsg, primary) => h('button', { class: `btn${primary ? ' btn-primary' : ''}`, type: 'button', onclick: async () => {
    if (confirmMsg && !confirm(confirmMsg)) return;
    try {
      await scanLibrary(mode);
      toast(`${label} started`);
    } catch (e) {
      toast(`${label} failed: ${e.message}`, 'error');
    }
  } }, label);
  scanSection.append(
    h('h2', { id: 'set-scan' }, 'Scan'),
    scanStatus,
    h('div', { class: 'button-row' },
      scanBtn('Rescan library', 'changes', null, true),
      scanBtn('Rescan playlists', 'playlists'),
      scanBtn('Clear library and rescan', 'full', 'This deletes the library database and rebuilds it from scratch. Continue?')));

  function renderScan() {
    const s = store.server || {};
    const lib = s.library || {};
    const scanning = !!s.scan;
    const done = s.scan?.done || 0, total = s.scan?.total || 0;
    const stats = h('dl', { class: 'stats' },
      stat('Albums', lib.albums), stat('Artists', lib.artists),
      stat('Tracks', lib.tracks), stat('Genres', lib.genres));
    if (scanning) {
      const pct = total ? Math.round((done / total) * 100) : null;
      scanStatus.replaceChildren(
        h('p', {}, h('span', { class: 'spinner inline' }), ' Scanning', s.scan.step ? `: ${humanize(s.scan.step)}` : '…'),
        h('progress', { max: total || 1, value: total ? done : null, 'aria-label': 'Scan progress' }),
        h('p', { class: 'muted small' }, total ? `${done.toLocaleString()} of ${total.toLocaleString()} (${pct}%)` : 'Preparing…'),
        stats);
    } else {
      const last = s.last_scan ? new Date(s.last_scan * 1000).toLocaleString() : 'never';
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

  // ---------------------------------------------------------------- this browser
  function renderBrowser() {
    const head = h('h2', { id: 'set-browser' }, 'This browser');
    const formats = playableFormats();
    if (!formats.length || typeof WebSocket === 'undefined') {
      browserSection.replaceChildren(head, h('p', { class: 'muted' }, 'This browser can’t play the server’s audio formats.'));
      return;
    }
    const checkId = fieldId();
    const preciseId = fieldId();
    const nameId = fieldId();
    const nameInput = h('input', { id: nameId, type: 'text', value: browserPlayer.name(), maxlength: 64 });
    const state = browserPlayer.enabled()
      ? (browserPlayer.connected() ? 'Playing here as a player of the server.' : 'Connecting…')
      : 'Off.';
    browserSection.replaceChildren(head,
      h('p', { class: 'muted small' }, 'Play music on this computer or phone: it becomes a player like the others, with the queue, sync and every remote control. Keep this page open while it plays.'),
      h('div', { class: 'field check' },
        h('input', { id: checkId, type: 'checkbox', checked: browserPlayer.enabled(), onchange: (e) => {
          setBrowserPlayer(e.target.checked, nameInput.value || undefined);
          toast(e.target.checked ? 'This browser is now a player' : 'This browser no longer plays');
          renderBrowser();
        } }),
        h('label', { for: checkId }, 'Play in this browser', h('span', { class: 'muted small block' }, state))),
      h('form', { class: 'field inline', onsubmit: (e) => {
        e.preventDefault();
        const v = nameInput.value.trim();
        if (!v) return;
        setBrowserPlayer(browserPlayer.enabled(), v);
        toast(`This browser plays as “${v}”`);
      } },
      h('label', { for: nameId }, 'Name as a player'),
      h('div', { class: 'inline-row' }, nameInput, h('button', { class: 'btn', type: 'submit' }, 'Save'))),
      ...(webAudioSupported() ? [h('div', { class: 'field check' },
        h('input', { id: preciseId, type: 'checkbox', checked: browserPlayer.precise(), onchange: (e) => {
          setBrowserPlayer(browserPlayer.enabled(), undefined, e.target.checked);
          renderBrowser();
        } }),
        h('label', { for: preciseId }, 'Keep in sync with other players',
          h('span', { class: 'muted small block' }, 'Plays through Web Audio, in step with the players it is synced with. Turn off to play through the browser’s own player, which some phones keep playing with the screen off.')))] : []),
      ...(browserPlayer.precise() ? [qualityField()] : []),
      h('p', { class: 'muted small' }, browserPlayer.precise() ? audioNote() : `Formats: ${formats.join(', ')}.`));
  }

  function qualityField() {
    const id = fieldId();
    return h('div', { class: 'field' },
      h('label', { for: id }, 'Audio over the network'),
      h('select', { id, onchange: (e) => { setBrowserPlayer(browserPlayer.enabled(), undefined, undefined, e.target.value); renderBrowser(); } },
        [['lossless', 'Lossless (FLAC)'], ['compact', 'Compact (Opus, for mobile data and slow Wi-Fi)']]
          .map(([v, l]) => h('option', { value: v, selected: browserPlayer.quality() === v }, l))));
  }

  function audioNote() {
    const d = browserPlayer.decodes;
    if (!browserPlayer.connected()) return '';
    if (!d.length) {
      return location.protocol === 'https:'
        ? 'This browser can’t decode FLAC or Opus: audio comes as raw samples (about 10 MB a minute).'
        : 'Audio comes as raw samples (about 10 MB a minute): on the secure address (see Secure connection) it comes as FLAC or Opus instead.';
    }
    if (browserPlayer.quality() === 'compact') {
      return d.includes('ops') ? 'Audio comes as Opus (about 1 MB a minute), or FLAC while synced with a player that doesn’t take Opus.' : 'This browser can’t decode Opus: audio comes as FLAC.';
    }
    return d.includes('flc') ? 'Audio comes as FLAC (about 5 MB a minute).' : 'Audio comes as raw samples.';
  }

  // ---------------------------------------------------------------- other players
  async function renderDevices() {
    const head = h('h2', { id: 'set-devices' }, 'Other players');
    let settings;
    try { settings = await getSettings(); } catch (e) {
      if (alive) devicesSection.replaceChildren(head, h('p', { class: 'muted', role: 'alert' }, `Could not load: ${e.message}`));
      return;
    }
    if (!alive) return;
    const kinds = [
      ['upnp_players', 'UPnP / DLNA renderers', 'Network speakers, receivers and TVs that play from a media server.'],
      ['chromecast_players', 'Chromecast', 'Chromecast and Google Cast speakers and TVs.'],
      ['airplay_players', 'AirPlay', 'AirPlay speakers and receivers (AirPlay 1).'],
    ];
    devicesSection.replaceChildren(head,
      h('p', { class: 'muted small' }, 'Devices on the network that aren’t Squeezeboxes can join as players: the queue, sync and every remote control work on them.'),
      ...kinds.map(([key, label, help]) => {
        const id = fieldId();
        return h('div', { class: 'field check' },
          h('input', { id, type: 'checkbox', checked: !!settings[key], onchange: async (e) => {
            await saveServer({ [key]: e.target.checked }, e.target.checked ? `${label} devices will join as players` : `${label} devices no longer play`);
          } }),
          h('label', { for: id }, label, h('span', { class: 'muted small block' }, help)));
      }));
  }

  // ---------------------------------------------------------------- wire up
  renderBrowser();
  renderDevices();
  // ---------------------------------------------------------------- secure connection
  let secureKey = null;
  function renderSecure() {
    const s = store.server;
    if (!s) return;
    const secure = location.protocol === 'https:';
    const key = `${secure}|${s.https_port}|${s.local_ca}`;
    if (key === secureKey) return;
    secureKey = key;
    const head = h('h2', { id: 'set-secure' }, 'Secure connection');
    const caLink = (label) => h('a', { href: `${serverRoot}tls/rms-ca.crt`, download: 'rms-ca.crt' }, label);
    const howTo = h('details', { class: 'small' },
      h('summary', {}, 'How to install it'),
      h('ul', {},
        h('li', {}, h('strong', {}, 'iPhone, iPad: '), 'open the link in Safari and allow the download; Settings → Profile Downloaded → Install; then Settings → General → About → Certificate Trust Settings → turn it on.'),
        h('li', {}, h('strong', {}, 'Android: '), 'Settings → Security → More security settings → Install from device storage → CA certificate.'),
        h('li', {}, h('strong', {}, 'Mac: '), 'open the file to add it to Keychain Access, open it there and set Trust → “Always Trust”.'),
        h('li', {}, h('strong', {}, 'Windows: '), 'open the file → Install Certificate → Local Machine → “Trusted Root Certification Authorities”.'),
        h('li', {}, h('strong', {}, 'Firefox, Linux: '), 'Firefox settings → Privacy & Security → View Certificates → Authorities → Import; on Linux also add it to the system’s store for Chrome.')),
      h('p', { class: 'muted' }, 'The certificate can only vouch for addresses on your own network (this server, .local and .lan names, private addresses), so installing it doesn’t let it stand in for other sites.'));
    const why = 'Over HTTPS this page can be installed as an app, and “Play in this browser” plays compressed audio with less battery.';
    if (secure) {
      secureSection.replaceChildren(head,
        h('p', {}, 'This page is on the server’s secure address.'),
        ...(s.local_ca ? [h('p', { class: 'muted small' }, 'For your other devices: install the server’s certificate (', caLink('rms-ca.crt'), ') on each one, then use the https address.'), howTo] : []));
    } else if (s.https_port) {
      const url = new URL(serverRoot, location.href);
      url.protocol = 'https:';
      url.port = String(s.https_port);
      secureSection.replaceChildren(head,
        h('p', { class: 'muted small' }, why),
        ...(s.local_ca ? [h('p', {}, '1. Install the server’s certificate on this device: ', caLink('download rms-ca.crt')), howTo] : []),
        h('p', {}, s.local_ca ? '2. Then open ' : 'Open ', h('a', { href: url.href }, url.href)));
    } else {
      secureSection.replaceChildren(head,
        h('p', { class: 'muted small' }, why),
        h('p', { class: 'muted small' }, 'HTTPS is off on this server (it was started with --https-port 0, or the port was taken).'));
    }
  }

  renderPlayer();
  renderSync(true);
  renderLibrary();
  renderSpotify();
  renderListenBrainz();
  renderScan();
  renderSecure();
  offs.push(
    on('player', () => { renderPlayer(); renderSync(true); }),
    on('players', () => { renderSync(); if (currentPlayer()?.name !== renderedName) renderPlayer(); }),
    on('status', () => renderSync()),
    on('server', () => { renderScan(); renderSecure(); }),
    on('browser-player', renderBrowser),
  );

  return () => { alive = false; clearInterval(spotifyPoll); offs.forEach((f) => f()); };
}

function appearance(ctx) {
  const id = fieldId();
  const sel = h('select', { id, onchange: (e) => ctx.setTheme(e.target.value) },
    [['auto', 'Match system'], ['light', 'Light'], ['dark', 'Dark']].map(([v, l]) => h('option', { value: v, selected: ctx.getTheme() === v }, l)));
  return h('section', { class: 'settings-card', 'aria-labelledby': 'set-look' },
    h('h2', { id: 'set-look' }, 'Appearance'),
    h('div', { class: 'field' }, h('label', { for: id }, 'Theme'), sel));
}

/** A select for a server `Choice` ({value, options: [{value, label}]}); onChange gets the value string. */
function choiceField(label, choice, onChange, help) {
  const id = fieldId();
  const known = choice.options.some((o) => o.value === choice.value);
  const opts = known ? choice.options : [...choice.options, { value: choice.value, label: choice.value }];
  return h('div', { class: 'field' },
    h('label', { for: id }, label),
    h('select', { id, onchange: (e) => onChange(e.target.value) },
      opts.map((o) => h('option', { value: o.value, selected: o.value === choice.value }, o.label))),
    help && h('p', { class: 'muted small' }, help));
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
