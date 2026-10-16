// RMS's Cast receiver app: plays the track RMS loads (an ordinary audio stream from the
// server), draws MilkDrop presets to it with projectM (WebAssembly, WebGL 2), and shows
// the track's cover and title.
//
// RMS sends its visualiser settings with each LOAD (customData.rms) and hears back on
// NS: how fast the TV draws (STATS), and what went wrong (ERROR), for its log.
//
// Open it in a browser with ?dev&src=<audio url>[&title=…&artist=…&album=…&art=<url>]
// to try it without a Chromecast; add &debug for the frame rate on screen.
(() => {
  'use strict';
  const NS = 'urn:x-cast:net.rms.visualizer';
  const PROTOCOL = 1;
  const params = new URLSearchParams(location.search);
  const dev = params.has('dev');
  const $ = (id) => document.getElementById(id);
  const canvas = $('canvas');
  const media = $('media');

  const settings = { presetSeconds: 30, fps: 30, renderSize: '1280x720' };
  let send = () => {};

  function report(type, data) {
    try { send(Object.assign({ type }, data)); } catch (e) { /* no sender connected */ }
  }
  const reported = new Set();
  function fail(message) {
    console.warn('visualiser:', message);
    if (reported.has(message)) return;
    reported.add(message);
    report('ERROR', { message });
  }

  // ---------------------------------------------------------------- projectM
  let pm = null;
  let ready = false;          // projectM started (pm is set a little before)
  let presets = [];
  let order = [];
  let pos = -1;
  let presetName = '';
  let failures = 0;

  function shuffle(a) {
    for (let i = a.length - 1; i > 0; i--) {
      const j = Math.floor(Math.random() * (i + 1));
      [a[i], a[j]] = [a[j], a[i]];
    }
    return a;
  }

  async function nextPreset(smooth) {
    if (!presets.length) return;
    pos = (pos + 1) % order.length;
    const p = presets[order[pos]];
    try {
      const text = await (await fetch(`presets/${p.file}`)).text();
      const ptr = pm.stringToNewUTF8(text);
      pm._pm_load(ptr, smooth ? 1 : 0);
      pm._free(ptr);
      presetName = p.name;
      failures = 0;
    } catch (e) {
      fail(`couldn't fetch preset ${p.name}`);
    }
  }

  function renderSize() {
    const m = /^(\d+)x(\d+)$/.exec(settings.renderSize || '');
    const w = m ? Number(m[1]) : 1280, h = m ? Number(m[2]) : 720;
    // no larger than the screen
    const sw = Math.round(canvas.clientWidth * (window.devicePixelRatio || 1)) || w;
    const sh = Math.round(canvas.clientHeight * (window.devicePixelRatio || 1)) || h;
    const k = Math.min(1, sw / w, sh / h);
    return [Math.max(1, Math.round(w * k)), Math.max(1, Math.round(h * k))];
  }

  function size() {
    const [w, h] = renderSize();
    if (canvas.width !== w || canvas.height !== h) {
      canvas.width = w;
      canvas.height = h;
      if (pm) pm._pm_resize(w, h);
    }
  }

  function applySettings(s) {
    if (!s || typeof s !== 'object') return;
    if (s.version && s.version > PROTOCOL) {
      fail(`RMS speaks receiver protocol ${s.version}; this receiver knows ${PROTOCOL}`);
    }
    if (Number(s.presetSeconds) > 0) settings.presetSeconds = Number(s.presetSeconds);
    if (Number(s.fps) > 0) settings.fps = Number(s.fps);
    if (typeof s.renderSize === 'string') settings.renderSize = s.renderSize;
    if (ready) {
      pm._pm_preset_seconds(settings.presetSeconds);
      pm._pm_fps(settings.fps);
    }
  }

  async function startProjectM() {
    if (typeof createPmweb !== 'function') throw new Error('pmweb.js didn’t load');
    pm = await createPmweb({ canvas });
    const manifest = await (await fetch('manifest.json')).json();
    presets = manifest.presets;
    order = shuffle(presets.map((_, i) => i));
    pm.FS.mkdir('/textures');
    await Promise.all(manifest.textures.map(async (t) => {
      try {
        const buf = new Uint8Array(await (await fetch(`textures/${t}`)).arrayBuffer());
        pm.FS.writeFile(`/textures/${t}`, buf);
      } catch (e) { /* a missing texture only looks plainer */ }
    }));
    size();
    const rc = pm._pm_init(canvas.width, canvas.height, 48, 32);
    if (rc === -1) throw new Error('this device has no WebGL 2');
    if (rc !== 0) throw new Error('projectM didn’t start');
    ready = true;
    pm.onSwitchRequested = () => nextPreset(true);
    pm.onSwitchFailed = (msg) => {
      console.warn('preset failed:', msg);
      if (++failures < 10) nextPreset(false);
    };
    applySettings(settings);
    await nextPreset(false);
    requestAnimationFrame(frame);
  }

  // --------------------------------------------------------------- the audio
  // The media element plays as usual until Web Audio is running; only then is it routed
  // through the analysers (once routed, it plays only through the audio context).
  let actx = null, left = null, right = null, bufL = null, bufR = null;
  let lastTime = 0, pcmPtr = 0, heard = false, playingSince = 0;
  const FFT = 2048;

  async function hookAudio() {
    if (actx) {
      if (actx.state !== 'running') await actx.resume().catch(() => {});
      return;
    }
    const ctx = new (window.AudioContext || window.webkitAudioContext)();
    await ctx.resume().catch(() => {});
    if (ctx.state !== 'running') {
      ctx.close();
      fail('Web Audio wouldn’t start, so the visuals can’t hear the music');
      return;
    }
    actx = ctx;
    const src = actx.createMediaElementSource(media);
    src.connect(actx.destination);
    const split = actx.createChannelSplitter(2);
    src.connect(split);
    left = actx.createAnalyser();
    right = actx.createAnalyser();
    left.fftSize = right.fftSize = FFT;
    split.connect(left, 0);
    split.connect(right, 1);
    bufL = new Float32Array(FFT);
    bufR = new Float32Array(FFT);
    lastTime = actx.currentTime;
  }

  function feedAudio() {
    if (!actx || !ready) return;
    const now = actx.currentTime;
    let n = Math.round((now - lastTime) * actx.sampleRate);
    lastTime = now;
    if (n <= 0) return;
    n = Math.min(n, FFT);
    left.getFloatTimeDomainData(bufL);
    right.getFloatTimeDomainData(bufR);
    if (!pcmPtr) pcmPtr = pm._malloc(FFT * 2 * 4);
    const heap = new Float32Array(pm.HEAPF32.buffer, pcmPtr, n * 2);
    const from = FFT - n;
    let peak = 0;
    for (let i = 0; i < n; i++) {
      const l = bufL[from + i], r = bufR[from + i];
      heap[2 * i] = l;
      heap[2 * i + 1] = r;
      peak = Math.max(peak, Math.abs(l), Math.abs(r));
    }
    pm._pm_pcm(pcmPtr, n);
    // all silence for a while: the browser won't let the page read the stream
    if (peak > 0) heard = true;
    else if (!heard && playingSince && performance.now() - playingSince > 8000 && media.volume > 0) {
      fail('the music reads as silence: the browser may be blocking access to the stream (CORS)');
    }
  }

  // ------------------------------------------------------------------ drawing
  let frames = 0, statsAt = performance.now(), lastDraw = 0, fps = 0;
  const debugEl = $('debug');
  debugEl.hidden = !params.has('debug');

  function frame(t) {
    requestAnimationFrame(frame);
    // at the server's frame rate setting, not the screen's
    if (t - lastDraw < 1000 / settings.fps - 4) return;
    lastDraw = t;
    size();
    feedAudio();
    pm._pm_render();
    frames++;
    const now = performance.now();
    if (now - statsAt >= 15000) {
      fps = Math.round(frames * 1000 / (now - statsAt));
      frames = 0;
      statsAt = now;
      report('STATS', { fps, size: `${canvas.width}x${canvas.height}`, preset: presetName });
    }
    if (!debugEl.hidden && frames % 15 === 0) {
      debugEl.textContent = `${fps || '…'} fps · ${canvas.width}×${canvas.height}\n${presetName}`;
    }
    updateProgress();
  }

  // -------------------------------------------------------------- track info
  const SMALL_AFTER = 12000;
  let smallTimer = null;
  let artUrl = '';
  let duration = 0;

  function state(name) {
    document.body.classList.toggle('playing', name === 'playing');
    document.body.classList.toggle('paused', name === 'paused');
    $('status').textContent = name === 'paused' ? 'Paused' : '';
  }

  function big(forMs) {
    document.body.classList.remove('small');
    clearTimeout(smallTimer);
    if (forMs) smallTimer = setTimeout(() => document.body.classList.add('small'), forMs);
  }

  function showTrack(meta, length) {
    meta = meta || {};
    const track = $('track');
    track.classList.add('changing');
    setTimeout(() => {
      $('title').textContent = meta.title || '';
      $('artist').textContent = meta.artist || meta.subtitle || meta.albumArtist || '';
      $('album').textContent = meta.albumName || '';
      const url = (meta.images && meta.images[0] && meta.images[0].url) || '';
      if (url !== artUrl) {
        artUrl = url;
        const art = $('art');
        art.classList.toggle('none', !url);
        if (url) art.src = url; else art.removeAttribute('src');
      }
      track.classList.remove('changing');
    }, 300);
    duration = Number(length) || 0;
    big(SMALL_AFTER);
  }

  $('art').addEventListener('error', () => $('art').classList.add('none'));
  $('art').addEventListener('load', () => accentFrom($('art')));

  // the progress bar takes a colour from the cover
  function accentFrom(img) {
    try {
      const c = document.createElement('canvas');
      c.width = c.height = 16;
      const g = c.getContext('2d');
      g.drawImage(img, 0, 0, 16, 16);
      const d = g.getImageData(0, 0, 16, 16).data;
      let best = null, bestScore = -1;
      for (let i = 0; i < d.length; i += 4) {
        const r = d[i], gr = d[i + 1], b = d[i + 2];
        const max = Math.max(r, gr, b), min = Math.min(r, gr, b);
        const score = (max - min) * (max / 255);       // saturated and bright
        if (score > bestScore) { bestScore = score; best = [r, gr, b]; }
      }
      if (best && bestScore > 40) {
        // lift dark colours so the bar reads on the dark shade
        const k = Math.max(1, 170 / Math.max(...best));
        const [r, gr, b] = best.map((v) => Math.min(255, Math.round(v * k)));
        document.documentElement.style.setProperty('--accent', `rgb(${r}, ${gr}, ${b})`);
        return;
      }
    } catch (e) { /* a cover without CORS can't be read: keep the default */ }
    document.documentElement.style.removeProperty('--accent');
  }

  function clock(s) {
    s = Math.max(0, Math.floor(s));
    const m = Math.floor(s / 60);
    return `${m}:${String(s % 60).padStart(2, '0')}`;
  }

  let lastProgress = 0;
  function updateProgress() {
    const now = performance.now();
    if (now - lastProgress < 250) return;
    lastProgress = now;
    const d = (isFinite(media.duration) && media.duration > 0) ? media.duration : duration;
    const t = media.currentTime || 0;
    $('bar').style.width = d ? `${Math.min(100, (t / d) * 100)}%` : '0';
    $('time').textContent = d ? `${clock(t)} / ${clock(d)}` : clock(t);
  }

  media.addEventListener('playing', () => {
    state('playing');
    playingSince = playingSince || performance.now();
    if (document.body.classList.contains('paused-before')) big(SMALL_AFTER);
    document.body.classList.remove('paused-before');
    hookAudio();
  });
  media.addEventListener('pause', () => {
    if (media.ended) return;
    state('paused');
    document.body.classList.add('paused-before');
    big(0);
  });
  media.addEventListener('emptied', () => { playingSince = 0; heard = false; });

  // ------------------------------------------------------------------ start
  function startCast() {
    const framework = cast.framework;
    const context = framework.CastReceiverContext.getInstance();
    const player = context.getPlayerManager();
    player.setMediaElement(media);
    player.setMessageInterceptor(framework.messages.MessageType.LOAD, (req) => {
      applySettings(req.customData && req.customData.rms);
      return req;
    });
    player.addEventListener(framework.events.EventType.PLAYER_LOAD_COMPLETE, () => {
      const info = player.getMediaInformation() || {};
      showTrack(info.metadata, info.duration);
    });
    const options = new framework.CastReceiverOptions();
    options.customNamespaces = { [NS]: framework.system.MessageType.JSON };
    // plain audio streams: no adaptive-streaming player needed
    options.skipPlayersLoad = true;
    options.statusText = 'RMS visualiser';
    context.start(options);
    send = (data) => context.sendCustomMessage(NS, undefined, data);
    $('idle-hint').textContent = 'Waiting for music';
  }

  function startDev() {
    const src = params.get('src');
    $('idle-hint').textContent = src ? 'Loading…' : 'Add ?dev&src=<audio url> to play something';
    if (!src) return;
    media.src = src;
    media.loop = true;
    showTrack({
      title: params.get('title') || src.split('/').pop(),
      artist: params.get('artist') || '',
      albumName: params.get('album') || '',
      images: params.get('art') ? [{ url: params.get('art') }] : [],
    });
    media.play().catch(() => {
      $('idle-hint').textContent = 'Click to start';
      window.addEventListener('click', () => media.play(), { once: true });
    });
  }

  if (dev) startDev();
  else if (window.cast && cast.framework) startCast();
  else $('idle-hint').textContent = 'The Cast receiver framework didn’t load';

  startProjectM().catch((e) => {
    fail(`the visualiser didn’t start: ${e.message || e}`);
  });
})();
