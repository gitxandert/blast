// The player's ring of stereo frames, shared by the AudioWorklet (HTTPS)
// and the ScriptProcessor fallback (plain HTTP, where browsers don't offer
// worklets).
//
// It waits until `target` frames are buffered before playing (absorbing
// network jitter). On an underrun it ramps to silence, raises `target`
// (the network just proved it stalls longer than the buffer covers) and
// buffers again, then fades back in. If the ring grows well past the target
// (the listener's clock runs slower than the server's) it crossfades ahead,
// so latency never creeps up. The user's buffer setting is the floor of
// `target`; it grows up to `MAX_S` and resets when the setting changes or
// listening restarts.
//
// Every cut is ramped: a hard jump in the waveform is a click, and a train
// of underruns and skips is heard as a broken rhythm.

const FADE_S = 0.004;
const MAX_S = 1.0;

export class Ring {
  constructor(rate, report) {
    this.rate = rate;
    this.report = report;
    this.cap = rate * 4;
    this.l = new Float32Array(this.cap);
    this.r = new Float32Array(this.cap);
    this.read = 0;
    this.write = 0;
    this.fill = 0;
    this.base = Math.round(rate * 0.12);
    this.target = this.base;
    this.fade = Math.max(1, Math.round(rate * FADE_S));
    this.playing = false;
    // fade in at the start of the next played block
    this.fadeIn = true;
    // the last frame played, for ramping down from it on an underrun
    this.lastL = 0;
    this.lastR = 0;
    this.underruns = 0;
    this.skipped = 0;
    this.sinceReport = 0;
  }

  onMessage(m) {
    if (m.type === 'pcm') {
      const { l, r } = m;
      for (let i = 0; i < l.length; i++) {
        if (this.fill >= this.cap) {
          // full: drop the oldest frame
          this.read = (this.read + 1) % this.cap;
          this.fill--;
        }
        this.l[this.write] = l[i];
        this.r[this.write] = r[i];
        this.write = (this.write + 1) % this.cap;
        this.fill++;
      }
    } else if (m.type === 'target') {
      this.base = Math.max(128, Math.min(this.rate * MAX_S, Math.round(m.frames)));
      this.target = this.base;
    } else if (m.type === 'reset') {
      this.read = this.write = this.fill = 0;
      this.playing = false;
      this.fadeIn = true;
      this.target = this.base;
    }
  }

  /// Fills `L` and `R` (the same array for mono output).
  render(L, R) {
    const n = L.length;
    const stereo = R !== L;
    if (!this.playing && this.fill >= this.target) {
      this.playing = true;
      this.fadeIn = true;
    }

    if (this.playing) {
      // the listener's clock is slow (or a burst came in after an
      // underrun): jump to `target`, crossfading from where we were
      let from = -1;
      if (this.fill > this.target * 2 + 2048) {
        const drop = this.fill - this.target;
        from = this.read;
        this.read = (this.read + drop) % this.cap;
        this.fill -= drop;
        this.skipped += drop;
      }
      // play what there is; on an underrun the rest ramps down from the
      // last frame played
      const have = Math.min(n, this.fill);
      const fadeIn = this.fadeIn ? Math.min(this.fade, have) : 0;
      const xfade = from >= 0 ? Math.min(this.fade, have) : 0;
      for (let i = 0; i < have; i++) {
        let l = this.l[this.read];
        let r = this.r[this.read];
        if (i < xfade) {
          const g = (i + 1) / (xfade + 1);
          const j = (from + i) % this.cap;
          l = l * g + this.l[j] * (1 - g);
          r = r * g + this.r[j] * (1 - g);
        }
        if (i < fadeIn) {
          const g = (i + 1) / (fadeIn + 1);
          l *= g;
          r *= g;
        }
        L[i] = l;
        if (stereo) R[i] = r;
        this.read = (this.read + 1) % this.cap;
      }
      this.fill -= have;
      this.fadeIn = false;
      if (have > 0) {
        this.lastL = L[have - 1];
        this.lastR = stereo ? R[have - 1] : this.lastL;
      }
      if (have < n) {
        const ramp = Math.min(this.fade, n - have);
        for (let i = have; i < n; i++) {
          const k = i - have;
          const g = k < ramp ? 1 - (k + 1) / (ramp + 1) : 0;
          L[i] = this.lastL * g;
          if (stereo) R[i] = this.lastR * g;
        }
        this.lastL = this.lastR = 0;
        this.playing = false;
        this.underruns++;
        this.target = Math.min(this.rate * MAX_S, Math.round(this.target * 1.5));
      }
    } else {
      L.fill(0);
      if (stereo) R.fill(0);
    }

    this.sinceReport += n;
    if (this.sinceReport >= this.rate / 4) {
      this.sinceReport = 0;
      this.report({
        fill: this.fill,
        bufferedMs: (this.fill / this.rate) * 1000,
        targetMs: (this.target / this.rate) * 1000,
        underruns: this.underruns,
        skipped: this.skipped,
        playing: this.playing,
      });
    }
  }
}
