// The audio thread's end of the stream (see ring.js for the buffering).

import { Ring } from './ring.js';

class BlastPlayer extends AudioWorkletProcessor {
  constructor() {
    super();
    this.ring = new Ring(sampleRate, (s) => this.port.postMessage(s));
    this.port.onmessage = (e) => this.ring.onMessage(e.data);
  }

  process(_inputs, outputs) {
    const out = outputs[0];
    this.ring.render(out[0], out[1] || out[0]);
    return true;
  }
}

registerProcessor('blast-player', BlastPlayer);
