// MilkDrop visuals drawn by projectM (WebAssembly, WebGL 2) to the audio Web Audio plays,
// and the playing track shown over them.  Shared by the web UI (views/visualiser.js) and
// RMS's Cast receiver (receiver/, which copies this file when it's built).
//
// projectM itself (pmweb.js, pmweb.wasm), the presets and the textures come from `base`:
// the receiver's own directory on the TV, and the receiver's GitHub Pages site for the web
// UI (see tools/projectm-viz/web and receiver/).

/** Where the web UI gets projectM, the presets and the textures. */
export const ASSETS = 'https://rustydc.github.io/RMS/receiver/';

/** Samples analysed each frame (the newest of them go to projectM). */
const FFT = 2048;

const scripts = new Map();

/** Load pmweb.js from `base` once; resolves to its module factory. */
function loadProjectM(base) {
  if (!scripts.has(base)) {
    scripts.set(base, new Promise((resolve, reject) => {
      if (typeof window.createPmweb === 'function') { resolve(window.createPmweb); return; }
      const s = document.createElement('script');
      s.src = `${base}pmweb.js`;
      s.async = true;
      s.onload = () => (typeof window.createPmweb === 'function'
        ? resolve(window.createPmweb) : reject(new Error('pmweb.js didn’t define projectM')));
      s.onerror = () => { scripts.delete(base); reject(new Error(`couldn’t load projectM from ${base}`)); };
      document.head.append(s);
    }));
  }
  return scripts.get(base);
}

function shuffle(a) {
  for (let i = a.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [a[i], a[j]] = [a[j], a[i]];
  }
  return a;
}

let canvasIds = 0;

/**
 * projectM drawing on a canvas, fed from a Web Audio node.
 *
 * Options: `base` (where projectM and the presets are), `presetSeconds`, `fps` (0: the
 * screen's rate), `size` ('1280x720': draw at that size, never larger than the canvas on
 * screen; empty: the canvas's size on screen, times `scale`), `scale` (0.25–1),
 * `maxPixels` (a cap on what `size` empty draws, for heavy presets on big screens);
 * `onPreset(name)`, `onStats({fps, size, preset})` every 15 seconds, and `onError(message)`.
 */
export class Visualiser {
  constructor(canvas, options = {}) {
    this.canvas = canvas;
    if (!canvas.id) canvas.id = `rms-viz-${++canvasIds}`;
    this.base = options.base || ASSETS;
    this.options = { presetSeconds: 30, fps: 0, size: '', scale: 1, maxPixels: 1920 * 1080, ...options };
    this.pm = null;
    this.ready = false;
    this.stopped = false;
    this.paused = false;
    this.presets = [];
    this.order = [];
    this.pos = -1;
    this.preset = '';
    this.failures = 0;
    this.audio = null;
    /** When the audio last had anything in it (performance.now()), 0: never. */
    this.heardAt = 0;
    this.frames = 0;
    this.statsAt = performance.now();
    this.lastDraw = 0;
    this.fps = 0;
    this.raf = 0;
    this.pcm = 0;
  }

  /** Load projectM, the presets and the textures, and start drawing. */
  async start() {
    const create = await loadProjectM(this.base);
    if (this.stopped) return;
    this.pm = await create({ canvas: this.canvas, locateFile: (f) => this.base + f });
    const manifest = await (await fetch(`${this.base}manifest.json`)).json();
    if (this.stopped) return;
    this.presets = manifest.presets;
    this.order = shuffle(this.presets.map((_, i) => i));
    const pm = this.pm;
    pm.FS.mkdir('/textures');
    await Promise.all(manifest.textures.map(async (t) => {
      try {
        const buf = new Uint8Array(await (await fetch(`${this.base}textures/${t}`)).arrayBuffer());
        pm.FS.writeFile(`/textures/${t}`, buf);
      } catch (e) { /* a missing texture only looks plainer */ }
    }));
    if (this.stopped) return;
    this.resize();
    const target = pm.stringToNewUTF8(`#${this.canvas.id}`);
    const rc = pm._pm_init(target, this.canvas.width, this.canvas.height, 48, 32);
    pm._free(target);
    if (rc === -1) throw new Error('this browser has no WebGL 2');
    if (rc !== 0) throw new Error('projectM didn’t start');
    this.ready = true;
    pm.onSwitchRequested = () => this.next(true);
    pm.onSwitchFailed = (msg) => {
      console.warn('visualiser: preset failed:', msg);
      if (++this.failures < 10) this.next(false);
    };
    this.setOptions({});
    await this.next(false);
    this.raf = requestAnimationFrame((t) => this.frame(t));
  }

  /** Change options (see the constructor). */
  setOptions(o) {
    Object.assign(this.options, o || {});
    if (!this.ready) return;
    this.pm._pm_preset_seconds(this.options.presetSeconds);
    this.pm._pm_fps(this.options.fps || 60);
  }

  /**
   * Feed it from `node` (any node of `ctx` that carries the audio).  Analysers are added
   * beside whatever the node already feeds; the node's output doesn't change.
   */
  setAudio(ctx, node) {
    if (this.audio?.ctx === ctx && this.audio?.node === node) return;
    this.clearAudio();
    const split = ctx.createChannelSplitter(2);
    const left = ctx.createAnalyser();
    const right = ctx.createAnalyser();
    left.fftSize = right.fftSize = FFT;
    node.connect(split);
    split.connect(left, 0);
    split.connect(right, 1);
    this.audio = {
      ctx, node, split, left, right, last: ctx.currentTime,
      l: new Float32Array(FFT), r: new Float32Array(FFT),
    };
  }

  clearAudio() {
    const a = this.audio;
    if (!a) return;
    try { a.node.disconnect(a.split); } catch (e) { /* already gone with its context */ }
    this.audio = null;
  }

  /** The next preset (blended in when `smooth`). */
  async next(smooth = false) {
    if (!this.ready || !this.presets.length) return;
    this.pos = (this.pos + 1) % this.order.length;
    const p = this.presets[this.order[this.pos]];
    try {
      const text = await (await fetch(`${this.base}presets/${p.file}`)).text();
      if (this.stopped) return;
      const ptr = this.pm.stringToNewUTF8(text);
      this.pm._pm_load(ptr, smooth ? 1 : 0);
      this.pm._free(ptr);
      this.preset = p.name;
      this.failures = 0;
      this.options.onPreset?.(p.name);
    } catch (e) {
      this.options.onError?.(`couldn’t fetch preset ${p.name}`);
    }
  }

  /** Stop drawing for a while (the canvas keeps its last frame), or start again. */
  setPaused(paused) {
    this.paused = !!paused;
  }

  /** Stop drawing and let go of the GPU. */
  stop() {
    this.stopped = true;
    cancelAnimationFrame(this.raf);
    this.clearAudio();
    if (this.ready) {
      if (this.pcm) this.pm._free(this.pcm);
      this.canvas.getContext('webgl2')?.getExtension('WEBGL_lose_context')?.loseContext();
    }
    this.ready = false;
    this.pm = null;
  }

  resize() {
    const c = this.canvas;
    const dpr = window.devicePixelRatio || 1;
    const sw = Math.max(1, Math.round((c.clientWidth || 1280) * dpr));
    const sh = Math.max(1, Math.round((c.clientHeight || 720) * dpr));
    let w, h;
    const m = /^(\d+)x(\d+)$/.exec(this.options.size || '');
    if (m) {
      w = Number(m[1]);
      h = Number(m[2]);
      const k = Math.min(1, sw / w, sh / h);
      w *= k;
      h *= k;
    } else {
      const s = Math.min(1, Math.max(0.25, this.options.scale || 1));
      w = sw * s;
      h = sh * s;
      const k = Math.min(1, Math.sqrt(this.options.maxPixels / (w * h)));
      w *= k;
      h *= k;
    }
    w = Math.max(1, Math.round(w));
    h = Math.max(1, Math.round(h));
    if (c.width !== w || c.height !== h) {
      c.width = w;
      c.height = h;
      if (this.ready) this.pm._pm_resize(w, h);
    }
  }

  /** The samples played since the last frame, into projectM. */
  feed() {
    const a = this.audio;
    if (!a) return;
    const now = a.ctx.currentTime;
    let n = Math.round((now - a.last) * a.ctx.sampleRate);
    a.last = now;
    if (n <= 0) return;
    n = Math.min(n, FFT);
    a.left.getFloatTimeDomainData(a.l);
    a.right.getFloatTimeDomainData(a.r);
    const pm = this.pm;
    if (!this.pcm) this.pcm = pm._malloc(FFT * 2 * 4);
    const heap = new Float32Array(pm.HEAPF32.buffer, this.pcm, n * 2);
    const from = FFT - n;
    let peak = 0;
    for (let i = 0; i < n; i++) {
      const l = a.l[from + i], r = a.r[from + i];
      heap[2 * i] = l;
      heap[2 * i + 1] = r;
      peak = Math.max(peak, Math.abs(l), Math.abs(r));
    }
    pm._pm_pcm(this.pcm, n);
    if (peak > 0) this.heardAt = performance.now();
  }

  frame(t) {
    if (this.stopped) return;
    this.raf = requestAnimationFrame((t2) => this.frame(t2));
    if (this.paused) {
      this.frames = 0;
      this.statsAt = performance.now();
      return;
    }
    const fps = this.options.fps;
    if (fps && t - this.lastDraw < 1000 / fps - 4) return;
    this.lastDraw = t;
    this.resize();
    this.feed();
    this.pm._pm_render();
    this.frames++;
    const now = performance.now();
    if (now - this.statsAt >= 15000) {
      this.fps = Math.round(this.frames * 1000 / (now - this.statsAt));
      this.frames = 0;
      this.statsAt = now;
      this.options.onStats?.({
        fps: this.fps, size: `${this.canvas.width}x${this.canvas.height}`, preset: this.preset,
      });
    }
  }
}

// ------------------------------------------------------------------ track info

const OVERLAY_CSS = `
.rms-track-shade {
  position: absolute; inset: auto 0 0 0; height: 45%; pointer-events: none;
  background: linear-gradient(to top, rgba(0, 0, 0, 0.55), transparent);
  opacity: 0; transition: opacity 0.8s ease;
}
.rms-track-shade.on { opacity: 1; }
.rms-track-shade.on.small { opacity: 0.6; }
.rms-track {
  --accent: #8fb4ff;
  position: absolute; left: var(--rms-safe-x, 5%); bottom: var(--rms-safe-y, 5%);
  max-width: calc(100% - 2 * var(--rms-safe-x, 5%));
  display: flex; align-items: flex-end; gap: clamp(14px, 2.2vw, 28px);
  color: #f2f3fb; font-family: "Familjen Grotesk", "Roboto", ui-sans-serif, system-ui, sans-serif;
  transition: opacity 0.6s ease; opacity: 0; pointer-events: none;
}
.rms-track.on { opacity: 1; }
.rms-track img {
  width: clamp(88px, 18.75vw, 240px); height: clamp(88px, 18.75vw, 240px); flex: none;
  border-radius: 10px; object-fit: cover; background: linear-gradient(135deg, #263056, #121626);
  box-shadow: 0 18px 50px rgba(0, 0, 0, 0.55);
  transition: width 0.8s cubic-bezier(.2, .7, .2, 1), height 0.8s cubic-bezier(.2, .7, .2, 1);
}
.rms-track img.none { display: none; }
.rms-track .t { min-width: 0; padding-bottom: 6px; display: grid; gap: 6px; transition: opacity 0.3s ease; }
.rms-track.changing .t { opacity: 0; }
.rms-track .status {
  font-size: clamp(11px, 1.2vw, 15px); letter-spacing: 0.14em; text-transform: uppercase;
  color: var(--accent); font-weight: 600; min-height: 1.2em;
}
.rms-track .title {
  font-size: clamp(22px, 3.6vw, 46px); line-height: 1.08; font-weight: 600; text-wrap: balance;
  display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden;
  text-shadow: 0 2px 18px rgba(0, 0, 0, 0.5);
}
.rms-track .artist { font-size: clamp(16px, 2.2vw, 28px); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.rms-track .album {
  font-size: clamp(14px, 1.7vw, 22px); color: rgba(222, 226, 245, 0.72);
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.rms-track .progress { display: flex; align-items: center; gap: 14px; margin-top: 10px; }
.rms-track .bar { width: clamp(140px, 33vw, 420px); height: 4px; border-radius: 2px; background: rgba(255, 255, 255, 0.18); overflow: hidden; }
.rms-track .bar > div { height: 100%; width: 0; background: var(--accent); border-radius: 2px; }
.rms-track .time { font-size: clamp(12px, 1.3vw, 17px); color: rgba(222, 226, 245, 0.72); font-variant-numeric: tabular-nums; }
.rms-track.small img { width: clamp(52px, 6.9vw, 88px); height: clamp(52px, 6.9vw, 88px); border-radius: 6px; }
.rms-track.small { gap: clamp(10px, 1.4vw, 18px); }
.rms-track.small .status, .rms-track.small .album, .rms-track.small .progress { display: none; }
.rms-track.small .title { font-size: clamp(16px, 1.9vw, 24px); -webkit-line-clamp: 1; }
.rms-track.small .artist { font-size: clamp(13px, 1.4vw, 18px); color: rgba(222, 226, 245, 0.72); }
.rms-track.small .t { gap: 2px; padding-bottom: 4px; }
@media (prefers-reduced-motion: reduce) { .rms-track, .rms-track img, .rms-track-shade { transition: none; } }
`;

function el(tag, cls) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  return e;
}

function clock(s) {
  s = Math.max(0, Math.floor(s || 0));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

/**
 * The playing track over the visuals, in the lower left of `parent` (which should be
 * positioned): large when a track starts or pauses, then a small card.
 */
export class TrackOverlay {
  constructor(parent, { smallAfter = 12000 } = {}) {
    if (!document.getElementById('rms-track-css')) {
      const style = el('style');
      style.id = 'rms-track-css';
      style.textContent = OVERLAY_CSS;
      document.head.append(style);
    }
    this.smallAfter = smallAfter;
    this.shade = el('div', 'rms-track-shade');
    this.root = el('div', 'rms-track');
    this.img = el('img', 'none');
    this.img.alt = '';
    this.img.crossOrigin = 'anonymous';
    this.img.addEventListener('error', () => this.img.classList.add('none'));
    this.img.addEventListener('load', () => this.accentFrom(this.img));
    const t = el('div', 't');
    this.statusEl = el('div', 'status');
    this.titleEl = el('div', 'title');
    this.artistEl = el('div', 'artist');
    this.albumEl = el('div', 'album');
    const progress = el('div', 'progress');
    const bar = el('div', 'bar');
    this.barEl = el('div');
    bar.append(this.barEl);
    this.timeEl = el('div', 'time');
    progress.append(bar, this.timeEl);
    t.append(this.statusEl, this.titleEl, this.artistEl, this.albumEl, progress);
    this.root.append(this.img, t);
    parent.append(this.shade, this.root);
    this.key = null;
    this.art = '';
    this.timer = 0;
    this.current = 'idle';
  }

  /** Show a track: {title, artist, album, art}; the same one again changes nothing. */
  track(t) {
    t = t || {};
    const key = [t.title, t.artist, t.album].join('\u0001');
    if (key === this.key) {
      this.setArt(t.art);
      return;
    }
    const first = this.key === null;
    this.key = key;
    this.root.classList.add('changing');
    setTimeout(() => {
      this.titleEl.textContent = t.title || '';
      this.artistEl.textContent = t.artist || '';
      this.albumEl.textContent = t.album || '';
      this.setArt(t.art);
      this.root.classList.remove('changing');
    }, first ? 0 : 300);
    this.big(this.smallAfter);
  }

  setArt(url) {
    url = url || '';
    if (url === this.art) return;
    this.art = url;
    this.img.classList.toggle('none', !url);
    if (url) this.img.src = url; else this.img.removeAttribute('src');
  }

  /** 'playing', 'paused' or 'idle' (hidden). */
  state(s) {
    if (s === this.current) return;
    const was = this.current;
    this.current = s;
    const on = s === 'playing' || s === 'paused';
    this.root.classList.toggle('on', on);
    this.shade.classList.toggle('on', on);
    this.statusEl.textContent = s === 'paused' ? 'Paused' : '';
    if (s === 'paused') this.big(0);
    else if (s === 'playing' && was === 'paused') this.big(this.smallAfter);
  }

  /** Where the track is (seconds), and its length if known. */
  progress(t, d) {
    this.barEl.style.width = d ? `${Math.min(100, (t / d) * 100)}%` : '0';
    this.timeEl.textContent = d ? `${clock(t)} / ${clock(d)}` : clock(t);
  }

  big(forMs) {
    this.root.classList.remove('small');
    this.shade.classList.remove('small');
    clearTimeout(this.timer);
    if (forMs) {
      this.timer = setTimeout(() => {
        this.root.classList.add('small');
        this.shade.classList.add('small');
      }, forMs);
    }
  }

  // the progress bar takes a colour from the cover
  accentFrom(img) {
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
        const score = (max - min) * (max / 255);        // saturated and bright
        if (score > bestScore) { bestScore = score; best = [r, gr, b]; }
      }
      if (best && bestScore > 40) {
        // lift dark colours so the bar reads on the dark shade
        const k = Math.max(1, 170 / Math.max(...best));
        const [r, gr, b] = best.map((v) => Math.min(255, Math.round(v * k)));
        this.root.style.setProperty('--accent', `rgb(${r}, ${gr}, ${b})`);
        return;
      }
    } catch (e) { /* a cover without CORS can't be read: keep the default */ }
    this.root.style.removeProperty('--accent');
  }

  destroy() {
    clearTimeout(this.timer);
    this.root.remove();
    this.shade.remove();
  }
}
