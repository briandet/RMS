// A player on Web Audio: plays what a bridge's renderer messages say (ToRenderer: load,
// preload, pause, resume at a moment, hold, skip, stop, volume) and says what it does
// (FromRenderer: ready, started, progress with its clock, fetched, ended, ...), keeping
// time like a Squeezebox.  The server's FLAC, Opus or raw samples are fetched and decoded
// here (decoders.js: WebCodecs where there is it), and an AudioWorklet (pcm-worklet.js;
// without one a ScriptProcessorNode) plays them on the audio clock: it starts at the
// moment it is told, plays silence or skips samples when told to, and the next track
// follows without a gap.  Its clock is performance.now(), and its play points are when
// the audio is heard (getOutputTimestamp, or the output's latency).
//
// Used by the browser player (browser-player.js) and RMS's Cast receiver (receiver/,
// which copies it with its modules when it's built).

import { PcmOutput } from './pcm-output.js';
import { pcmDecoder, codecDecoder, decodableFormats } from './decoders.js';

/** @typedef {import('./api-types').ToRenderer} ToRenderer */

/**
 * Options: `report(msg)` (a FromRenderer message), `absolute(url)` (the server's URL for a
 * server-relative one), `rate()` (the sample rate to open the audio context at),
 * `needGesture(retry)` (the browser wants a click before it plays) and `blocked()`
 * (waiting for that click).
 */
export function createWebAudioPlayer(options) {
  const report = options.report;
  const absolute = options.absolute;

  /** Start playing once this much is buffered (seconds); report ready at this much. */
  const START_AFTER = 0.5;
  const READY_AFTER = 1.0;
  /** Fetch no further ahead than this (seconds): a track is not held in memory whole. */
  const AHEAD = 30;

  /** @type {AudioContext|null} */
  let ctx = null;
  /** Sends a message to the output (pcm-output.js). */
  let post = () => {};
  /** @type {GainNode|null} */
  let gain = null;
  /** The output node (worklet or script processor), before the volume. */
  let out = null;
  let loading = null;
  let sr = 44100;
  let volume = 1;

  let seq = 0;
  /** The track playing, or loaded and waiting to start: {id, autostart, started, ready, fetched}. */
  let current = null;
  /** The track after it, preloaded. */
  let next = null;
  /** Aborts the fetches of what is loaded. */
  let abort = new AbortController();
  /** Fetches run one after another, in queue order. */
  let fetching = Promise.resolve();
  let fetchesRunning = 0;
  let paused = false;
  /** The last word from the worklet on where output is. */
  let pos = null;
  /** Frames sent to the worklet and not yet played, as far as known here. */
  let buffered = 0;
  let lastProgress = 0;

  /** Create the audio context and the worklet (once). */
  function setup() {
    if (loading) return loading;
    // Opus decodes at 48 kHz; FLAC and raw samples are mostly 44.1 kHz
    const rate = options.rate();
    ctx = new AudioContext({ sampleRate: rate, latencyHint: 'playback' });
    sr = ctx.sampleRate;
    gain = ctx.createGain();
    gain.gain.value = volume;
    gain.connect(ctx.destination);
    if (ctx.audioWorklet && typeof AudioWorkletNode !== 'undefined') {
      loading = ctx.audioWorklet.addModule(new URL('./pcm-worklet.js', import.meta.url)).then(() => {
        const node = new AudioWorkletNode(ctx, 'rms-pcm', { numberOfInputs: 0, outputChannelCount: [2] });
        node.connect(gain);
        out = node;
        node.port.onmessage = (e) => fromOutput(e.data);
        post = (m, transfer) => node.port.postMessage(m, transfer || []);
      });
    } else {
      // no AudioWorklet (not https): the same output on the page's own thread
      const pcm = new PcmOutput(fromOutput, 1);
      const node = ctx.createScriptProcessor(2048, 1, 2);
      node.onaudioprocess = (e) => {
        const b = e.outputBuffer;
        pcm.render(b.getChannelData(0), b.getChannelData(1), Math.round(e.playbackTime * sr));
      };
      node.connect(gain);
      out = node;
      post = (m) => pcm.message(m);
      loading = Promise.resolve();
    }
    return loading;
  }

  /** The audio context must run to play: browsers allow that after a click or key. */
  function running() {
    if (!ctx || ctx.state === 'running') return;
    ctx.resume().then(() => {
      if (ctx.state !== 'running') options.needGesture(running);
    }).catch(() => options.needGesture(running));
    // a context that stays suspended needs a click
    setTimeout(() => { if (ctx.state === 'suspended' && !options.blocked()) options.needGesture(running); }, 500);
  }

  /**
   * A moment of the audio clock and when it is heard: `contextTime` seconds of the context
   * are heard at `performanceTime` (performance.now() time).
   */
  function timestamp() {
    const ts = ctx.getOutputTimestamp?.();
    if (ts?.performanceTime) return ts;
    // estimated: what is rendered now is heard after the output's latency
    return {
      contextTime: ctx.currentTime,
      performanceTime: performance.now() + ((ctx.outputLatency || 0) + (ctx.baseLatency || 0)) * 1000,
    };
  }

  /** The context frame heard at `ms` (performance.now() time). */
  function frameAt(ms) {
    const ts = timestamp();
    return Math.round((ts.contextTime + (ms - ts.performanceTime) / 1000) * sr);
  }

  /** When context frame `frame` is heard (performance.now() time). */
  function heardAt(frame) {
    const ts = timestamp();
    return ts.performanceTime + (frame / sr - ts.contextTime) * 1000;
  }

  /** Seconds into the current track heard at `now`. */
  function elapsed(now) {
    if (!pos || !current || pos.track !== current.id) return 0;
    let e = pos.played / sr;
    if (pos.playing) e += (now - heardAt(pos.frame)) / 1000;
    return Math.max(0, e);
  }

  function progress(force) {
    const now = performance.now();
    if (!current?.started || (!force && now - lastProgress < 1000)) return;
    lastProgress = now;
    report({ type: 'progress', elapsed: elapsed(now), jiffies: Math.round(now) >>> 0 });
  }

  function fromOutput(m) {
    switch (m.type) {
      case 'pos':
        pos = m;
        buffered = m.buffered;
        if (m.playing && !paused) progress(false);
        break;
      case 'track':
        if (next && m.track === next.id) {
          current = next;
          next = null;
        }
        if (current && m.track === current.id && !current.started) {
          current.started = true;
          pos = null;
          report({ type: 'started' });
          if (current.fetched) report({ type: 'fetched' });
        }
        break;
      case 'drained':
        // the end, unless more is on its way (then it is only an underrun)
        if (current?.started && !paused && fetchesRunning === 0) {
          progress(true);
          report({ type: 'ended' });
          current = null;
        }
        break;
      default:
        break;
    }
  }

  /** Start the current track now, or at `at` (milliseconds of performance.now()). */
  function start(at) {
    running();
    paused = false;
    post({ type: 'start', frame: at === undefined ? 0 : frameAt(at) });
  }

  const sleep = (ms) => new Promise((r) => { setTimeout(r, ms); });

  /** Fetch track `t`'s audio, decode it and send the samples to the worklet. */
  async function fetchTrack(t, url, format, pcm, signal) {
    // (anything else would play as noise: a sync group with no format every player takes)
    if (format !== 'flc' && format !== 'ops' && format !== 'pcm') {
      const decodes = [...await decodableFormats(), 'pcm'];
      if (!signal.aborted) report({ type: 'error', message: `can't play ${format}: this browser plays ${decodes.join(', ')}` });
      return;
    }
    let res;
    try {
      res = await fetch(absolute(url), { signal });
      if (!res.ok || !res.body) throw new Error(`HTTP ${res.status}`);
    } catch (e) {
      if (!signal.aborted) report({ type: 'error', message: e.message });
      return;
    }
    let total = 0;
    const deliver = (left, right) => {
      if (signal.aborted) return;
      // (counted first: posting hands the buffers over, and leaves them empty here)
      total += left.length;
      buffered += left.length;
      post({ type: 'audio', track: t.id, left, right }, [left.buffer, right.buffer]);
      // enough to begin
      if (t === current && !t.started && !t.ready) {
        if (t.autostart && total >= START_AFTER * sr) { t.ready = true; start(); }
        if (!t.autostart && total >= READY_AFTER * sr) { t.ready = true; report({ type: 'ready' }); }
      }
    };
    const fail = (e) => { if (!signal.aborted) report({ type: 'error', message: e.message || String(e) }); };
    const decoder = format === 'pcm'
      ? pcmDecoder(pcm || { bits: 16, rate: 44100, channels: 2, little_endian: true }, sr, deliver)
      : codecDecoder(format, sr, deliver, fail);
    const reader = res.body.getReader();
    try {
      for (;;) {
        // keep only so much ahead of what plays
        while (buffered > AHEAD * sr && !signal.aborted) await sleep(250);
        const { done, value } = await reader.read();
        if (signal.aborted) return;
        if (done) break;
        await decoder.push(value);
      }
      await decoder.end();
    } catch (e) {
      fail(e);
      return;
    }
    if (signal.aborted) return;
    if (t === current && !t.started && !t.ready) {
      // a short track: all there is
      t.ready = true;
      if (t.autostart) start();
      else report({ type: 'ready' });
    }
    // the next track can come (once this one plays: until then, it is the next one)
    t.fetched = true;
    if (t.started) report({ type: 'fetched' });
  }

  function queueFetch(t, url, format, pcm) {
    const signal = abort.signal;
    fetching = fetching.then(async () => {
      if (signal.aborted) return;
      fetchesRunning++;
      try { await fetchTrack(t, url, format, pcm, signal); } finally { fetchesRunning--; }
    });
  }

  function stop() {
    abort.abort();
    abort = new AbortController();
    fetching = Promise.resolve();
    current = null;
    next = null;
    paused = false;
    pos = null;
    buffered = 0;
    post({ type: 'clear' });
  }

  /** @param {ToRenderer} msg */
  async function handle(msg) {
    await setup();
    switch (msg.type) {
      case 'load':
        stop();
        current = { id: ++seq, autostart: !!msg.autostart, started: false, ready: false, fetched: false };
        if (msg.autostart) running();
        queueFetch(current, msg.url, msg.format, msg.pcm);
        break;
      case 'preload':
        next = { id: ++seq, autostart: true, started: false, ready: true, fetched: false };
        queueFetch(next, msg.url, msg.format, msg.pcm);
        break;
      case 'pause':
        if (!current) break;
        progress(true);
        paused = true;
        post({ type: 'pause' });
        if (current.started) report({ type: 'paused' });
        break;
      case 'resume':
        if (!current) break;
        start(msg.at ?? undefined);
        if (current.started) report({ type: 'resumed' });
        break;
      case 'stop':
        stop();
        break;
      case 'hold':
        post({ type: 'hold', frames: Math.round((msg.ms * sr) / 1000) });
        break;
      case 'skip':
        post({ type: 'skip', frames: Math.round((msg.ms * sr) / 1000) });
        break;
      case 'volume': {
        const v = Math.max(0, Math.min(100, msg.volume)) / 100;
        volume = msg.muted ? 0 : v * v;
        gain?.gain.setTargetAtTime(volume, ctx.currentTime, 0.02);
        break;
      }
      default:
        break;
    }
  }

  return {
    /** The audio as it plays, before the volume (for the visualiser). */
    tap: () => (ctx && out ? { ctx, node: out } : null),
    /**
     * What it plays, best first (LMS codes), and what it decodes: `compact` asks for Opus
     * first (unless a synced player can't take it).
     */
    async query(compact = false) {
      const decodes = await decodableFormats();
      const has = (f) => decodes.includes(f);
      const formats = compact && has('ops') ? ['ops', 'flc', 'pcm']
        : has('flc') ? ['flc', 'pcm'] : ['pcm'];
      return { formats: formats.join(','), rate: String(sr), clock: '1', decodes };
    },
    /** Messages are handled in order, though setting up is asynchronous. */
    handle: (() => {
      let chain = Promise.resolve();
      return (msg) => { chain = chain.then(() => handle(msg)).catch((e) => report({ type: 'error', message: e.message })); };
    })(),
    stop,
    unlock() { setup().then(running).catch(() => {}); },
    /** Start again with a new audio context (its rate follows the quality). */
    reset() {
      stop();
      ctx?.close().catch(() => {});
      ctx = null;
      out = null;
      loading = null;
      post = () => {};
    },
    prepare: setup,
  };
}
