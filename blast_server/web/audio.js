// Listening in the browser: an AudioContext, a player (the `blast-player`
// worklet over HTTPS, a ScriptProcessor over plain HTTP, where browsers
// offer no worklets), and a volume. `feed` takes the server's binary audio
// messages (layout in src/engine/stream.rs).

import { Ring } from './ring.js';

const SILENT_WAV =
  'data:audio/wav;base64,UklGRiQAAABXQVZFZm10IBAAAAABAAEARKwAAIhYAQACABAAZGF0YQAAAAA=';

export class Listener {
  constructor() {
    this.ctx = null;
    this.node = null;
    this.gain = null;
    this.onstats = null;
    this.jitterMs = 120;
    this.volume = 0.9;
    this.streamRate = 0;
    this.lastSeq = -1;
    this.lost = 0;
    // linear resampler state, used only when the context can't run at the
    // stream's rate
    this.rs = { pos: 0, lastL: 0, lastR: 0 };
    this.keepAlive = null;
    this.port = null;
    this.kind = null;
  }

  get active() {
    return !!this.node;
  }

  /// Call from a user gesture (a tap): iOS only starts audio then.
  async start({ rate = 48000, jitterMs = 120, volume = 0.9 } = {}) {
    this.jitterMs = jitterMs;
    this.volume = volume;
    // iOS: play through the silent switch, like a media app
    try {
      if (navigator.audioSession) navigator.audioSession.type = 'playback';
    } catch (_) {}
    let ctx;
    try {
      ctx = new AudioContext({ sampleRate: rate, latencyHint: 'interactive' });
    } catch (_) {
      ctx = new AudioContext({ latencyHint: 'interactive' });
    }
    this.ctx = ctx;
    const resumed = ctx.resume();
    // older iOS: a playing <audio> element moves the page to the playback
    // category, so the ringer switch doesn't mute Web Audio
    if (!navigator.audioSession) {
      try {
        const a = new Audio(SILENT_WAV);
        a.loop = true;
        a.setAttribute('playsinline', '');
        a.play().catch(() => {});
        this.keepAlive = a;
      } catch (_) {}
    }
    const stats = (d) => this.onstats && this.onstats({ ...d, lost: this.lost, rate: ctx.sampleRate, kind: this.kind });
    if (ctx.audioWorklet && window.isSecureContext) {
      await ctx.audioWorklet.addModule('worklet.js');
      await resumed;
      this.node = new AudioWorkletNode(ctx, 'blast-player', {
        numberOfInputs: 0,
        numberOfOutputs: 1,
        outputChannelCount: [2],
      });
      this.port = this.node.port;
      this.port.onmessage = (e) => stats(e.data);
      this.kind = 'worklet';
    } else {
      // runs on the page's main thread: a bigger block rides out jank
      await resumed;
      const ring = new Ring(ctx.sampleRate, stats);
      this.node = ctx.createScriptProcessor(2048, 0, 2);
      this.node.onaudioprocess = (e) => {
        const b = e.outputBuffer;
        ring.render(b.getChannelData(0), b.numberOfChannels > 1 ? b.getChannelData(1) : b.getChannelData(0));
      };
      this.port = { postMessage: (m) => ring.onMessage(m) };
      this.kind = 'script';
    }
    this.gain = ctx.createGain();
    this.gain.gain.value = this.volume;
    this.node.connect(this.gain).connect(ctx.destination);
    this.setJitter(jitterMs);
    this.lastSeq = -1;
  }

  async stop() {
    const ctx = this.ctx;
    if (this.node) this.node.disconnect();
    this.node = null;
    this.port = null;
    this.gain = null;
    this.ctx = null;
    if (this.keepAlive) {
      this.keepAlive.pause();
      this.keepAlive = null;
    }
    if (ctx) await ctx.close().catch(() => {});
  }

  setVolume(v) {
    this.volume = v;
    if (this.gain) this.gain.gain.setTargetAtTime(v, this.ctx.currentTime, 0.02);
  }

  setJitter(ms) {
    this.jitterMs = ms;
    if (this.port) this.port.postMessage({ type: 'target', frames: (ms / 1000) * this.ctx.sampleRate });
  }

  /// One binary message from the socket.
  feed(buf) {
    if (!this.node) return;
    const dv = new DataView(buf);
    if (dv.getUint8(0) !== 1) return;
    const fmt = dv.getUint8(1);
    const ch = dv.getUint16(2, true);
    const rate = dv.getUint32(4, true);
    const seq = dv.getUint32(8, true);
    if (this.lastSeq >= 0 && seq !== ((this.lastSeq + 1) >>> 0)) this.lost++;
    this.lastSeq = seq;
    if (rate !== this.streamRate) {
      this.streamRate = rate;
      this.rs = { pos: 0, lastL: 0, lastR: 0 };
    }
    const samples = fmt === 1 ? new Float32Array(buf, 12) : new Int16Array(buf, 12);
    const scale = fmt === 1 ? 1 : 1 / 32768;
    const frames = Math.floor(samples.length / ch);
    let l = new Float32Array(frames);
    let r = new Float32Array(frames);
    for (let i = 0; i < frames; i++) {
      l[i] = samples[i * ch] * scale;
      r[i] = samples[i * ch + (ch > 1 ? 1 : 0)] * scale;
    }
    if (rate !== this.ctx.sampleRate) [l, r] = this.resample(l, r, rate, this.ctx.sampleRate);
    if (this.kind === 'worklet') this.port.postMessage({ type: 'pcm', l, r }, [l.buffer, r.buffer]);
    else this.port.postMessage({ type: 'pcm', l, r });
  }

  // linear interpolation, continuous across messages: position -1 is the
  // previous message's last frame
  resample(l, r, from, to) {
    const step = from / to;
    const s = this.rs;
    const n = l.length;
    const outL = [];
    const outR = [];
    while (s.pos < n - 1) {
      const i = Math.floor(s.pos);
      const f = s.pos - i;
      const l0 = i < 0 ? s.lastL : l[i];
      const r0 = i < 0 ? s.lastR : r[i];
      outL.push(l0 + (l[i + 1] - l0) * f);
      outR.push(r0 + (r[i + 1] - r0) * f);
      s.pos += step;
    }
    s.pos -= n;
    s.lastL = l[n - 1];
    s.lastR = r[n - 1];
    return [Float32Array.from(outL), Float32Array.from(outR)];
  }
}
