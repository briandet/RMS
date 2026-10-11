// Decoders for the Web Audio player (browser-player.js): a track's audio stream, as it
// arrives, into stereo float samples at the audio context's rate.  Raw samples (`pcm`) are
// converted here; FLAC and Opus are split into frames (demux.js) and decoded by the
// browser's WebCodecs AudioDecoder, where it has one (on secure pages).

import { FlacDemuxer, OggOpusDemuxer } from './demux.js';

/**
 * @typedef {{ push(bytes: Uint8Array): Promise<void>, end(): Promise<void> }} Decoder
 *   `push` the stream's bytes as they come, then `end`; both resolve once the decoder is
 *   ready for more.  Samples go to `deliver(left, right)` in order.
 */

/** Linear resampling from `from` Hz to `to` Hz, across the chunks of a stream. */
export function resampler(from, to) {
  const step = from / to;
  let t = 0;
  let prevL = 0;
  let prevR = 0;
  return (/** @type {Float32Array} */ left, /** @type {Float32Array} */ right) => {
    const frames = left.length;
    if (step === 1 || !frames) return { left, right };
    // input sample k is at position k + 1, the last one of the chunk before at 0
    const n = Math.max(0, Math.ceil((frames - t) / step));
    const outL = new Float32Array(n);
    const outR = new Float32Array(n);
    let j = 0;
    for (; j < n && t < frames; j++, t += step) {
      const i = Math.floor(t);
      const f = t - i;
      const aL = i === 0 ? prevL : left[i - 1];
      const aR = i === 0 ? prevR : right[i - 1];
      outL[j] = aL + (left[i] - aL) * f;
      outR[j] = aR + (right[i] - aR) * f;
    }
    t -= frames;
    prevL = left[frames - 1];
    prevR = right[frames - 1];
    return { left: outL.subarray(0, j), right: outR.subarray(0, j) };
  };
}

/**
 * Raw samples as the server describes them (`pcm`: bits, rate, channels, endianness).
 * @param {import('./api-types').Pcm} pcm
 * @returns {Decoder}
 */
export function pcmDecoder(pcm, rate, deliver) {
  const bytes = pcm.bits / 8;
  const channels = Math.max(1, pcm.channels);
  const frameBytes = bytes * channels;
  const le = pcm.little_endian;
  const resample = resampler(pcm.rate, rate);
  let rest = new Uint8Array(0);
  const sample = (v, o) => {
    switch (bytes) {
      case 1: return (v.getUint8(o) - 128) / 128;
      case 2: return v.getInt16(o, le) / 32768;
      case 3: {
        const b0 = v.getUint8(o);
        const b1 = v.getUint8(o + 1);
        const b2 = v.getUint8(o + 2);
        const x = le ? (b2 << 16) | (b1 << 8) | b0 : (b0 << 16) | (b1 << 8) | b2;
        return ((x << 8) >> 8) / 8388608;
      }
      default: return v.getInt32(o, le) / 2147483648;
    }
  };
  return {
    async push(chunk) {
      let data = chunk;
      if (rest.length) {
        data = new Uint8Array(rest.length + chunk.length);
        data.set(rest);
        data.set(chunk, rest.length);
      }
      const frames = Math.floor(data.length / frameBytes);
      rest = data.slice(frames * frameBytes);
      const v = new DataView(data.buffer, data.byteOffset, frames * frameBytes);
      const left = new Float32Array(frames);
      const right = new Float32Array(frames);
      for (let i = 0, o = 0; i < frames; i++, o += frameBytes) {
        left[i] = sample(v, o);
        right[i] = channels > 1 ? sample(v, o + bytes) : left[i];
      }
      const out = resample(left, right);
      if (out.left.length) deliver(out.left, out.right);
    },
    async end() {},
  };
}

/** Frames waiting in the AudioDecoder before `push` waits for it. */
const QUEUE = 16;

/**
 * FLAC (`flc`) or Ogg Opus (`ops`) through WebCodecs.
 * @returns {Decoder}
 */
export function codecDecoder(format, rate, deliver, onError) {
  const demux = format === 'ops' ? new OggOpusDemuxer() : new FlacDemuxer();
  /** @type {AudioDecoder|null} */
  let decoder = null;
  let failed = null;
  let resample = null;
  // Opus: the encoder's lead-in to leave out, and where the audio ends (padding after)
  let skip = 0;
  let out = 0;

  function output(/** @type {AudioData} */ d) {
    try {
      const n = d.numberOfFrames;
      let left = new Float32Array(n);
      d.copyTo(left, { planeIndex: 0, format: 'f32-planar' });
      let right = left;
      if (d.numberOfChannels > 1) {
        right = new Float32Array(n);
        d.copyTo(right, { planeIndex: 1, format: 'f32-planar' });
      }
      resample ||= resampler(d.sampleRate, rate);
      let from = 0;
      let to = n;
      if (skip > 0) {
        from = Math.min(skip, n);
        skip -= from;
      }
      const total = demux.totalSamples;
      if (format === 'ops' && total != null) to = Math.max(from, Math.min(n, from + total - out));
      out += to - from;
      if (from > 0 || to < n) {
        left = left.subarray(from, to);
        right = right.subarray(from, to);
      }
      const r = resample(left, right);
      if (r.left.length) deliver(r.left, r.right);
    } finally {
      d.close();
    }
  }

  async function feed() {
    if (failed) throw failed;
    if (!decoder && demux.config) {
      decoder = new AudioDecoder({ output, error: (e) => { failed = e; onError(e); } });
      decoder.configure(demux.config);
      if (format === 'ops') skip = demux.preSkip || 0;
    }
    if (!decoder) return;
    for (const f of demux.take()) {
      decoder.decode(new EncodedAudioChunk({ type: 'key', timestamp: f.timestamp, duration: f.duration, data: f.data }));
    }
    // let the decoder catch up rather than queueing the whole track in it
    while (decoder.decodeQueueSize > QUEUE && !failed) {
      await new Promise((r) => { decoder.addEventListener('dequeue', r, { once: true }); });
    }
    if (failed) throw failed;
  }

  return {
    async push(bytes) {
      demux.push(bytes);
      await feed();
    },
    async end() {
      demux.end();
      await feed();
      if (decoder && decoder.state === 'configured') {
        await decoder.flush();
        decoder.close();
      }
    },
  };
}

/** The STREAMINFO of a 16-bit stereo 44.1 kHz FLAC stream, to ask whether FLAC decodes. */
function flacDescription() {
  const d = new Uint8Array(4 + 4 + 34);
  d.set([0x66, 0x4c, 0x61, 0x43, 0x80, 0, 0, 34]);
  const v = new DataView(d.buffer, 8);
  v.setUint16(0, 4096);
  v.setUint16(2, 4096);
  // sample rate (20 bits), channels − 1 (3), bits − 1 (5), total samples (36)
  v.setUint32(10, (44100 << 12) | (1 << 9) | (15 << 4));
  return d;
}

/** Which of FLAC and Opus this browser decodes (WebCodecs), as LMS format codes. */
export async function decodableFormats() {
  if (typeof AudioDecoder === 'undefined' || typeof EncodedAudioChunk === 'undefined') return [];
  const ok = async (config) => {
    try { return (await AudioDecoder.isConfigSupported(config)).supported === true; } catch { return false; }
  };
  const out = [];
  if (await ok({ codec: 'flac', sampleRate: 44100, numberOfChannels: 2, description: flacDescription() })) out.push('flc');
  if (await ok({ codec: 'opus', sampleRate: 48000, numberOfChannels: 2 })) out.push('ops');
  return out;
}
