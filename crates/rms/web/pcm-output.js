// The Web Audio player's output (see browser-player.js): plays queued stereo samples, starts
// at a given frame of the audio clock, and plays silence or skips audio when told to, so the
// server can keep it in step with other players.  It runs in an AudioWorklet
// (pcm-worklet.js), or where there is none on the page, in a ScriptProcessorNode.
//
// Messages in ({@link PcmOutput#message}):
//   {type: 'audio', track, left, right}  samples (Float32Array) for a track, in order
//   {type: 'start', frame}               play from that context frame on (0: now)
//   {type: 'pause'} / {type: 'clear'}
//   {type: 'hold', frames} / {type: 'skip', frames}
// Messages out (to `emit`):
//   {type: 'pos', frame, track, played, buffered, playing}  every few blocks: `played` frames of
//                                         `track` have been output before context `frame`;
//                                         `playing`: output goes on from there
//   {type: 'track', track, frame}         a track's first sample is output at context `frame`
//   {type: 'drained'}                     playing, and nothing left to play

export class PcmOutput {
  constructor(emit, reportEvery = 8) {
    this.emit = emit;
    this.reportEvery = reportEvery;
    /** @type {{track: number, left: Float32Array, right: Float32Array, at: number}[]} */
    this.queue = [];
    this.buffered = 0;
    this.playing = false;
    this.startFrame = -1;
    this.hold = 0;
    this.skip = 0;
    this.track = -1;
    this.played = 0;
    this.blocks = 0;
    this.drained = false;
    /** The context frame output has reached. */
    this.frame = 0;
  }

  message(m) {
    switch (m.type) {
      case 'audio':
        this.queue.push({ track: m.track, left: m.left, right: m.right, at: 0 });
        this.buffered += m.left.length;
        this.drained = false;
        break;
      case 'start':
        this.startFrame = Math.max(m.frame, this.frame);
        this.playing = true;
        break;
      case 'pause':
        this.playing = false;
        this.startFrame = -1;
        break;
      case 'clear':
        this.queue = [];
        this.buffered = 0;
        this.playing = false;
        this.startFrame = -1;
        this.hold = 0;
        this.skip = 0;
        this.track = -1;
        this.played = 0;
        this.drained = false;
        break;
      case 'hold':
        this.hold += m.frames;
        break;
      case 'skip':
        this.skip += m.frames;
        break;
      default:
        break;
    }
  }

  /** Where output is at context `frame`. */
  report(frame) {
    this.emit({
      type: 'pos', frame, track: this.track, played: this.played, buffered: this.buffered,
      playing: this.playing && this.startFrame <= frame && this.hold === 0 && !this.drained,
    });
  }

  /** Drop up to `n` frames from the queue; returns how many were dropped. */
  drop(n, frame) {
    let done = 0;
    while (done < n && this.queue.length) {
      const c = this.queue[0];
      const k = Math.min(n - done, c.left.length - c.at);
      this.enter(c, frame);
      c.at += k;
      done += k;
      this.played += k;
      this.buffered -= k;
      if (c.at >= c.left.length) this.queue.shift();
    }
    return done;
  }

  /** Output reaches chunk `c`: a new track starts at context `frame`. */
  enter(c, frame) {
    if (c.track !== this.track) {
      this.track = c.track;
      this.played = 0;
      this.emit({ type: 'track', track: c.track, frame });
    }
  }

  /** Fill `left` and `right` (one block, starting at context `frame`). */
  render(left, right, frame) {
    const n = left.length;
    left.fill(0);
    if (right !== left) right.fill(0);
    let i = 0;
    if (!this.playing) {
      i = n;
    } else if (this.startFrame > frame) {
      // not yet: start within this block or a later one
      i = Math.min(n, this.startFrame - frame);
    }
    if (i < n && this.skip > 0) this.skip -= this.drop(this.skip, frame + i);
    while (i < n) {
      if (this.hold > 0) {
        const k = Math.min(this.hold, n - i);
        this.hold -= k;
        i += k;
        continue;
      }
      const c = this.queue[0];
      if (!c) {
        if (!this.drained) {
          this.drained = true;
          this.emit({ type: 'drained' });
        }
        break;
      }
      this.enter(c, frame + i);
      const k = Math.min(c.left.length - c.at, n - i);
      left.set(c.left.subarray(c.at, c.at + k), i);
      if (right !== left) right.set(c.right.subarray(c.at, c.at + k), i);
      c.at += k;
      i += k;
      this.played += k;
      this.buffered -= k;
      if (c.at >= c.left.length) this.queue.shift();
    }
    this.frame = frame + n;
    if (++this.blocks % this.reportEvery === 0) this.report(this.frame);
  }
}
