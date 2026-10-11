// Streaming demuxers for the browser player: turn an audio byte stream that arrives in arbitrary
// pieces (fetch() + response.body.getReader()) into frames for the WebCodecs AudioDecoder.
//
//   const d = new FlacDemuxer();          // or new OggOpusDemuxer()
//   d.push(bytes);                        // each piece of the stream, in order
//   d.end();                              // the stream is over: emit the last frame
//   d.take();                             // frames complete so far: {timestamp, duration, samples, data}
//   d.config;                             // null until the header is parsed, then for AudioDecoder.configure
//
// Timestamps and durations are in microseconds from the start of the stream, counted from the
// frames' sample counts (not from numbers coded in the stream).  push() and end() throw when the
// stream is not of the demuxer's kind.

/**
 * @typedef {{timestamp: number, duration: number, samples: number, data: Uint8Array}} Frame
 * @typedef {{codec: string, sampleRate: number, numberOfChannels: number, description: Uint8Array}} Config
 */

/**
 * A byte queue: data is appended at the end and dropped from the start.  Positions are stream
 * offsets; `buf[off - base]` holds the byte at stream offset `off`, for `start <= off < end`.
 * The buffer grows geometrically and is compacted in place, so appending is amortised linear.
 */
class ByteQueue {
  constructor() {
    this.buf = new Uint8Array(1 << 16);
    /** Stream offset of buf[0]. */
    this.base = 0;
    this.start = 0;
    this.end = 0;
    /** Bytes still to be dropped as they arrive (dropping past the end of the data). */
    this.skip = 0;
  }

  get length() {
    return this.end - this.start;
  }

  /** The byte at stream offset `off`. */
  at(off) {
    return this.buf[off - this.base];
  }

  push(bytes) {
    if (this.skip > 0) {
      // the queue is empty: skipped bytes are never copied
      const k = Math.min(this.skip, bytes.length);
      this.skip -= k;
      this.start += k;
      this.end += k;
      bytes = bytes.subarray(k);
    }
    if (this.start === this.end) this.base = this.start;
    if (!bytes.length) return;
    const live = this.end - this.start;
    if (this.end - this.base + bytes.length > this.buf.length) {
      const from = this.start - this.base;
      const need = live + bytes.length;
      if (need <= this.buf.length >> 1) {
        this.buf.copyWithin(0, from, from + live);
      } else {
        let size = this.buf.length;
        while (size < need * 2) size *= 2;
        const buf = new Uint8Array(size);
        buf.set(this.buf.subarray(from, from + live));
        this.buf = buf;
      }
      this.base = this.start;
    }
    this.buf.set(bytes, this.end - this.base);
    this.end += bytes.length;
  }

  /** Drop `n` bytes from the start (more than are here: drop the rest as they arrive). */
  drop(n) {
    const k = Math.min(n, this.length);
    this.start += k;
    this.skip += n - k;
  }

  /** A copy of the bytes from stream offset `a` to `b`. */
  slice(a, b) {
    return this.buf.slice(a - this.base, b - this.base);
  }
}

/** Microseconds at sample `n` of a stream at `rate` Hz. */
const usAt = (n, rate) => Math.round(n * 1e6 / rate);

function makeFrame(data, samples, before, rate) {
  const timestamp = usAt(before, rate);
  return { timestamp, duration: usAt(before + samples, rate) - timestamp, samples, data };
}

// ---- FLAC ----------------------------------------------------------------------------------------

const CRC8 = new Uint8Array(256);
const CRC16 = new Uint16Array(256);
for (let i = 0; i < 256; i++) {
  let c8 = i;
  let c16 = i << 8;
  for (let k = 0; k < 8; k++) {
    c8 = (c8 << 1 ^ (c8 & 0x80 ? 0x07 : 0)) & 0xff;
    c16 = (c16 << 1 ^ (c16 & 0x8000 ? 0x8005 : 0)) & 0xffff;
  }
  CRC8[i] = c8;
  CRC16[i] = c16;
}

const FLAC_RATES = [0, 88200, 176400, 192000, 8000, 16000, 22050, 24000, 32000, 44100, 48000, 96000];
const FLAC_BITS = [0, 8, 12, -1, 16, 20, 24, 32];
/** parseFlacHeader's result when the header runs past the data. */
const MORE = 0;

/**
 * Parse the FLAC frame header at `buf[i]` (data up to `end`); the caller has checked the sync
 * code.  Returns MORE, null (not a valid header), or what it says (rate and bits 0: as STREAMINFO).
 */
function parseFlacHeader(buf, i, end) {
  if (i + 5 > end) return MORE;
  const b2 = buf[i + 2];
  const b3 = buf[i + 3];
  const bsCode = b2 >> 4;
  const rateCode = b2 & 15;
  const chanCode = b3 >> 4;
  const bits = FLAC_BITS[b3 >> 1 & 7];
  if (bsCode === 0 || rateCode === 15 || chanCode > 10 || bits < 0 || b3 & 1) return null;
  // the frame or sample number, UTF-8 coded (up to 36 bits: 7 bytes)
  const u = buf[i + 4];
  let more = 0;
  if (u === 0xff || (u & 0xc0) === 0x80) return null;
  while (u << more & 0x40 && u & 0x80) more++;
  let p = i + 5;
  if (p + more > end) return MORE;
  for (let k = 0; k < more; k++, p++) if ((buf[p] & 0xc0) !== 0x80) return null;
  // optional block size and sample rate, then the CRC-8
  const extra = (bsCode === 6 ? 1 : bsCode === 7 ? 2 : 0) + (rateCode === 12 ? 1 : rateCode > 12 ? 2 : 0);
  if (p + extra + 1 > end) return MORE;
  let blockSize;
  if (bsCode === 1) blockSize = 192;
  else if (bsCode <= 5) blockSize = 576 << bsCode - 2;
  else if (bsCode === 6) blockSize = buf[p++] + 1;
  else if (bsCode === 7) blockSize = (buf[p++] << 8 | buf[p++]) + 1;
  else blockSize = 256 << bsCode - 8;
  let rate = FLAC_RATES[rateCode];
  if (rateCode === 12) rate = buf[p++] * 1000;
  else if (rateCode === 13) rate = buf[p++] << 8 | buf[p++];
  else if (rateCode === 14) rate = (buf[p++] << 8 | buf[p++]) * 10;
  let crc = 0;
  for (let k = i; k < p; k++) crc = CRC8[crc ^ buf[k]];
  if (crc !== buf[p]) return null;
  return {
    len: p + 1 - i, blockSize, rate, bits, variable: buf[i + 1] & 1,
    channels: chanCode < 8 ? chanCode + 1 : 2,
  };
}

/**
 * FLAC (native, optionally after an ID3v2 tag) → frames.  A frame ends where the next valid frame
 * header begins: one with a good CRC-8, that agrees with STREAMINFO, and where the bytes before
 * it give a good CRC-16, so sync codes inside audio data are not taken for frame starts.
 */
export class FlacDemuxer {
  constructor() {
    /** @type {Config | null} */
    this.config = null;
    /** Samples (per channel) in the stream, from STREAMINFO; null when not known. */
    this.totalSamples = null;
    this.q = new ByteQueue();
    /** 'head' (ID3 or fLaC), 'meta' (metadata blocks), 'frames' */
    this.state = 'head';
    this.ended = false;
    /** @type {Frame[]} */
    this.frames = [];
    this.info = null;
    this.samples = 0;
    // the pending frame: where it starts, its header, and its CRC-16 up to crcPos
    this.frameStart = -1;
    this.header = null;
    this.crc = 0;
    this.crcPos = 0;
    /** Where to go on looking for the next frame header. */
    this.scan = 0;
    // the first valid header with a bad CRC-16 in the pending frame: where to cut if the frame
    // grows too long (it was damaged)
    this.fallback = -1;
    this.fallbackHeader = null;
  }

  /** @param {Uint8Array} bytes */
  push(bytes) {
    this.q.push(bytes);
    this.process();
  }

  end() {
    this.ended = true;
    this.process();
    if (this.state === 'frames' && this.frameStart >= 0) this.finish();
  }

  /** @returns {Frame[]} */
  take() {
    const f = this.frames;
    this.frames = [];
    return f;
  }

  process() {
    const q = this.q;
    while (q.skip === 0) {
      if (this.state === 'head') {
        if (q.length < 4) return;
        const s = q.start;
        if (q.at(s) === 0x49 && q.at(s + 1) === 0x44 && q.at(s + 2) === 0x33) {
          // ID3v2: 10-byte header with a syncsafe size, then maybe a 10-byte footer
          if (q.length < 10) return;
          let size = 0;
          for (let k = 6; k < 10; k++) size = size * 128 + (q.at(s + k) & 0x7f);
          q.drop(10 + size + (q.at(s + 5) & 0x10 ? 10 : 0));
        } else if (q.at(s) === 0x66 && q.at(s + 1) === 0x4c && q.at(s + 2) === 0x61 && q.at(s + 3) === 0x43) {
          q.drop(4);
          this.state = 'meta';
        } else {
          throw new Error('not a FLAC stream');
        }
      } else if (this.state === 'meta') {
        if (q.length < 4) return;
        const s = q.start;
        const type = q.at(s) & 0x7f;
        const last = q.at(s) & 0x80;
        const len = q.at(s + 1) << 16 | q.at(s + 2) << 8 | q.at(s + 3);
        if (type === 0) {
          if (q.length < 4 + len) return;
          if (len < 34) throw new Error('bad FLAC STREAMINFO');
          this.streamInfo(q.slice(s + 4, s + 38));
        }
        q.drop(4 + len);
        if (last) {
          if (!this.info) throw new Error('no FLAC STREAMINFO');
          this.state = 'frames';
        }
      } else {
        if (this.frameStart < 0) this.scan = Math.max(this.scan, q.start);
        this.scanFrames();
        return;
      }
    }
  }

  streamInfo(si) {
    const rate = si[10] << 12 | si[11] << 4 | si[12] >> 4;
    const channels = (si[12] >> 1 & 7) + 1;
    const bits = ((si[12] & 1) << 4 | si[13] >> 4) + 1;
    const total = (si[13] & 15) * 2 ** 32 + (si[14] << 24 >>> 0) + (si[15] << 16 | si[16] << 8 | si[17]);
    const maxBlock = si[2] << 8 | si[3];
    const maxFrame = si[7] << 16 | si[8] << 8 | si[9];
    // the most a frame can take: STREAMINFO's max frame size, or a verbatim frame of the largest block
    const limit = Math.max(maxFrame, (maxBlock || 65536) * channels * Math.ceil((bits + 1) / 8) + 64);
    this.info = { rate, channels, bits, maxBlock, limit };
    this.totalSamples = total || null;
    const description = new Uint8Array(42);
    description.set([0x66, 0x4c, 0x61, 0x43, 0x80, 0, 0, 34]);
    description.set(si, 8);
    this.config = { codec: 'flac', sampleRate: rate, numberOfChannels: channels, description };
  }

  /** Whether frame header `h` fits this stream. */
  fits(h) {
    const info = this.info;
    return !!h && (!h.rate || h.rate === info.rate) && h.channels === info.channels
      && (!h.bits || h.bits === info.bits) && (info.maxBlock < 16 || h.blockSize <= info.maxBlock)
      && (!this.header || h.variable === this.header.variable);
  }

  scanFrames() {
    const q = this.q;
    const buf = q.buf;
    const base = q.base;
    const end = q.end - base;
    let i = this.scan - base;
    for (;;) {
      if (this.frameStart >= 0 && i + base - this.frameStart > this.info.limit && this.fallback >= 0) {
        // the pending frame is too long to be one (its CRC-16 is bad): cut it at the first header
        i = this.fallback - base;
        this.emit(this.fallback, this.fallbackHeader);
        i += this.header.len;
        continue;
      }
      i = buf.indexOf(0xff, i);
      if (i < 0 || i >= end) {
        i = end;
        break;
      }
      if (i + 1 >= end) break;
      if ((buf[i + 1] & 0xfe) !== 0xf8) {
        i++;
        continue;
      }
      const h = parseFlacHeader(buf, i, end);
      if (h === MORE && !this.ended) break;
      if (!this.fits(h)) {
        i++;
        continue;
      }
      if (this.frameStart < 0) {
        // the first frame (anything before it is not audio)
        q.start = i + base;
        this.emit(i + base, h);
      } else if (this.crcTo(i + base) === 0) {
        this.emit(i + base, h);
      } else {
        if (this.fallback < 0) {
          this.fallback = i + base;
          this.fallbackHeader = h;
        }
        i++;
        continue;
      }
      i += h.len;
    }
    this.scan = i + base;
  }

  /** The CRC-16 of the pending frame up to stream offset `to`. */
  crcTo(to) {
    const buf = this.q.buf;
    const base = this.q.base;
    let c = this.crc;
    for (let i = this.crcPos - base, n = to - base; i < n; i++) c = (c << 8 ^ CRC16[c >> 8 ^ buf[i]]) & 0xffff;
    this.crc = c;
    this.crcPos = to;
    return c;
  }

  /** Emit the pending frame (if any), which ends at stream offset `at`, where header `h` starts the next. */
  emit(at, h) {
    if (this.frameStart >= 0) {
      const n = this.header.blockSize;
      this.frames.push(makeFrame(this.q.slice(this.frameStart, at), n, this.samples, this.info.rate));
      this.samples += n;
    }
    this.q.start = at;
    this.frameStart = at;
    this.header = h;
    this.crc = 0;
    this.crcPos = at;
    this.fallback = -1;
  }

  /** At the end of the stream: the rest is the last frame, up to where its CRC-16 checks (if it does). */
  finish() {
    const buf = this.q.buf;
    const base = this.q.base;
    let c = this.crc;
    let last = -1;
    const min = this.frameStart + this.header.len + 2;
    for (let i = this.crcPos - base, n = this.q.end - base; i < n; i++) {
      c = (c << 8 ^ CRC16[c >> 8 ^ buf[i]]) & 0xffff;
      if (c === 0 && i + 1 + base >= min) last = i + 1 + base;
    }
    this.emit(last >= 0 ? last : this.q.end, null);
    this.frameStart = -1;
    this.q.start = this.q.end;
  }
}

// ---- Ogg Opus ------------------------------------------------------------------------------------

const CRC32 = new Uint32Array(256);
for (let i = 0; i < 256; i++) {
  let c = i << 24;
  for (let k = 0; k < 8; k++) c = c & 0x80000000 ? c << 1 ^ 0x04c11db7 : c << 1;
  CRC32[i] = c >>> 0;
}

/** Samples at 48 kHz in Opus packet `p`, from its TOC byte (RFC 6716 §3.1). */
export function opusPacketSamples(p) {
  if (!p.length) return 0;
  const config = p[0] >> 3;
  let frame;
  if (config < 12) frame = [480, 960, 1920, 2880][config & 3];  // SILK: 10/20/40/60 ms
  else if (config < 16) frame = config & 1 ? 960 : 480;           // hybrid: 10/20 ms
  else frame = 120 << (config & 3);                               // CELT: 2.5/5/10/20 ms
  const code = p[0] & 3;
  const count = code === 0 ? 1 : code < 3 ? 2 : p.length > 1 ? p[1] & 0x3f : 0;
  return frame * count;
}

/**
 * Ogg Opus → Opus packets (RFC 7845).  Reads the first logical stream only: pages of other
 * streams, and anything after its end-of-stream page, are ignored.  Pages with a bad CRC are
 * skipped (the next "OggS" is looked for).
 */
export class OggOpusDemuxer {
  constructor() {
    /** @type {Config | null} */
    this.config = null;
    /** Samples (at 48 kHz) to drop from the start of the decoded output. */
    this.preSkip = 0;
    /** Once the end-of-stream page is seen: samples of real audio (decoded output beyond is padding). */
    this.totalSamples = null;
    this.q = new ByteQueue();
    this.ended = false;
    this.done = false;
    /** @type {Frame[]} */
    this.frames = [];
    this.serial = -1;
    this.packets = 0;
    this.samples = 0;
    /** Pieces (copies) of a packet continued on the next page; null: skip to the next packet. */
    this.partial = [];
  }

  /** @param {Uint8Array} bytes */
  push(bytes) {
    if (this.done) return;
    this.q.push(bytes);
    this.process();
  }

  end() {
    this.ended = true;
    if (!this.done) this.process();
  }

  /** @returns {Frame[]} */
  take() {
    const f = this.frames;
    this.frames = [];
    return f;
  }

  process() {
    const q = this.q;
    while (!this.done && q.length >= 27) {
      const buf = q.buf;
      const s = q.start - q.base;
      if (buf[s] !== 0x4f || buf[s + 1] !== 0x67 || buf[s + 2] !== 0x67 || buf[s + 3] !== 0x53 || buf[s + 4]) {
        this.resync();
        continue;
      }
      const nseg = buf[s + 26];
      if (q.length < 27 + nseg) return;
      let len = 27 + nseg;
      for (let k = 0; k < nseg; k++) len += buf[s + 27 + k];
      if (q.length < len) return;
      let crc = 0;
      for (let k = 0; k < len; k++) {
        const b = k >= 22 && k < 26 ? 0 : buf[s + k];
        crc = (crc << 8 ^ CRC32[(crc >>> 24 ^ b) & 0xff]) >>> 0;
      }
      const want = (buf[s + 22] | buf[s + 23] << 8 | buf[s + 24] << 16 | buf[s + 25] << 24) >>> 0;
      if (crc !== want) {
        this.resync();
        continue;
      }
      this.page(s, nseg);
      q.drop(len);
    }
  }

  /** Skip to the next "OggS" after the start of the data. */
  resync() {
    const q = this.q;
    const buf = q.buf;
    let i = q.start - q.base + 1;
    const end = q.end - q.base;
    for (; (i = buf.indexOf(0x4f, i)) >= 0 && i < end; i++) {
      if (i + 3 >= end || buf[i + 1] === 0x67 && buf[i + 2] === 0x67 && buf[i + 3] === 0x53) break;
    }
    q.start = i < 0 || i > end ? q.end : i + q.base;
    this.partial = null;
  }

  /** Handle the page at `buf[s]` with `nseg` segments. */
  page(s, nseg) {
    const q = this.q;
    const buf = q.buf;
    const flags = buf[s + 5];
    const serial = (buf[s + 14] | buf[s + 15] << 8 | buf[s + 16] << 16 | buf[s + 17] << 24) >>> 0;
    if (this.serial < 0) {
      if (!(flags & 2)) return;  // not the start of a stream: wait for one
      this.serial = serial;
    } else if (serial !== this.serial) {
      return;
    }
    if (!(flags & 1) && this.partial && this.partial.length) this.partial = [];  // lost the rest of a packet
    if (flags & 1 && this.partial && !this.partial.length) this.partial = null;  // its start was lost
    let p = s + 27 + nseg;
    let from = p;
    for (let k = 0; k < nseg; k++) {
      const lace = buf[s + 27 + k];
      p += lace;
      if (lace === 255) continue;
      if (this.partial) {
        const piece = buf.subarray(from, p);
        if (this.partial.length) this.packet(concat([...this.partial, piece]));
        else this.packet(buf.slice(from, p));
      }
      this.partial = [];
      from = p;
    }
    if (from < p && this.partial) this.partial.push(buf.slice(from, p));
    if (flags & 4) {
      const lo = (buf[s + 6] | buf[s + 7] << 8 | buf[s + 8] << 16 | buf[s + 9] << 24) >>> 0;
      const hi = (buf[s + 10] | buf[s + 11] << 8 | buf[s + 12] << 16 | buf[s + 13] << 24) >>> 0;
      if (hi !== 0xffffffff) this.totalSamples = Math.max(0, hi * 2 ** 32 + lo - this.preSkip);
      this.done = true;
      this.q.start = this.q.end;
    }
  }

  packet(p) {
    const n = this.packets++;
    if (n === 0) {
      // OpusHead: magic, version, channels, pre-skip, input rate, gain, mapping
      if (p.length < 19 || String.fromCharCode(...p.subarray(0, 8)) !== 'OpusHead') {
        throw new Error('not an Ogg Opus stream');
      }
      this.preSkip = p[10] | p[11] << 8;
      this.inputSampleRate = (p[12] | p[13] << 8 | p[14] << 16 | p[15] << 24) >>> 0;
      this.config = { codec: 'opus', sampleRate: 48000, numberOfChannels: p[9], description: p };
    } else if (n > 1 && p.length) {
      // (the second packet is OpusTags)
      const samples = opusPacketSamples(p);
      this.frames.push(makeFrame(p, samples, this.samples, 48000));
      this.samples += samples;
    }
  }
}

function concat(parts) {
  let n = 0;
  for (const p of parts) n += p.length;
  const out = new Uint8Array(n);
  n = 0;
  for (const p of parts) {
    out.set(p, n);
    n += p.length;
  }
  return out;
}
