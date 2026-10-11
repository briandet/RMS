// The Web Audio player's output as an AudioWorklet: see pcm-output.js.

import { PcmOutput } from './pcm-output.js';

class PcmPlayer extends AudioWorkletProcessor {
  constructor() {
    super();
    this.out = new PcmOutput((m) => this.port.postMessage(m));
    this.port.onmessage = (e) => this.out.message(e.data);
  }

  process(_inputs, outputs) {
    const [left, right] = outputs[0];
    this.out.render(left, right || left, currentFrame);
    return true;
  }
}

registerProcessor('rms-pcm', PcmPlayer);
