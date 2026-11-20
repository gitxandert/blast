// blast server: the REPL in the browser. One WebSocket carries the REPL
// (lines, completions, history, the shared transcript) and, while
// listening, the engine's audio. Panels use the JSON API.

import { Listener } from './audio.js';

const $ = (s) => document.querySelector(s);
const $$ = (s) => [...document.querySelectorAll(s)];
function el(tag, attrs = {}, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === 'class') e.className = v;
    else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
    else if (v === true) e.setAttribute(k, '');
    else if (v !== false && v != null) e.setAttribute(k, v);
  }
  for (const k of kids.flat()) if (k != null) e.append(k.nodeType ? k : String(k));
  return e;
}

async function api(method, url, body, raw = false) {
  const opts = { method, headers: {} };
  if (body !== undefined) {
    if (raw) opts.body = body;
    else {
      opts.headers['content-type'] = 'application/json';
      opts.body = JSON.stringify(body);
    }
  }
  const r = await fetch(url, opts);
  const ct = r.headers.get('content-type') || '';
  const data = ct.includes('json') ? await r.json() : await r.text();
  if (!r.ok) {
    const err = new Error((data && data.error) || data || r.statusText);
    err.status = r.status;
    throw err;
  }
  return data;
}

let toastTimer;
function toast(msg, isErr = false) {
  const t = $('#toast');
  t.textContent = msg;
  t.className = isErr ? 'err' : '';
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.hidden = true), isErr ? 5000 : 2500);
}

const utf8len = (s) => new TextEncoder().encode(s).length;
const keepFocus = (e) => e.preventDefault(); // buttons that mustn't close the keyboard

// ---------------------------------------------------------------- state

const S = {
  me: null,
  prefs: {},
  ws: null,
  connected: false,
  pending: false,
  history: [],
  histKey: null,
  histIdx: 0,
  histSaved: '',
  reqId: 0,
  waiting: new Map(),
  cycle: null,
  chipsCtx: null,
  engine: null,
  sourcesDir: '',
  listening: false,
  wantListen: false,
  lastStatus: null,
};
const listener = new Listener();
const MARKERS = ['^', 'X', 'v', '>', 'X', '<', 'Z'];
let markerIdx = 0;

// ---------------------------------------------------------------- prefs

function applyPrefs() {
  const p = S.prefs;
  const root = document.documentElement;
  if (p.theme && p.theme !== 'auto') root.dataset.theme = p.theme;
  else delete root.dataset.theme;
  root.style.setProperty('--font', `${p.font_size || 14}px`);
  buildKeys();
  listener.setVolume(p.volume ?? 0.9);
  listener.setJitter(p.jitter_ms ?? 120);
  $('#vol').value = p.volume ?? 0.9;
  $('#jit').value = p.jitter_ms ?? 120;
  $('#jit-v').textContent = `${p.jitter_ms ?? 120} ms`;
  fillSettings();
}

async function savePrefs(changes, quiet = false) {
  try {
    S.prefs = await api('POST', '/api/prefs', changes);
    applyPrefs();
    if (!quiet) toast('Saved');
  } catch (e) {
    toast(e.message, true);
  }
}

// ---------------------------------------------------------------- transcript

const T = $('#transcript');
const MAX_LINES = 4000;

function nearBottom() {
  return T.scrollHeight - T.scrollTop - T.clientHeight < 80;
}
function scrollDown() {
  T.scrollTop = T.scrollHeight;
}

function addLine(kind, text, extra) {
  const stick = nearBottom();
  const d = el('div', { class: `ln ${kind}` }, text);
  if (extra) d.append(extra);
  T.append(d);
  while (T.childElementCount > MAX_LINES) T.firstElementChild.remove();
  if (stick) scrollDown();
}

function printEvent(ev) {
  if (ev.kind === 'echo') {
    const m = MARKERS[markerIdx];
    const who = ev.user && S.me && ev.user !== S.me.name ? el('span', { class: 'who' }, ev.user) : null;
    addLine('echo', `${m}${ev.cont ? '..' : ''} ${ev.text}`, who);
    return;
  }
  if (ev.kind === 'out') {
    // the REPL prints errors and warnings inside output too
    for (const block of splitKinds(ev.text)) addLine(block.kind, block.text);
    return;
  }
  addLine(ev.kind, ev.text);
}

function splitKinds(text) {
  const out = [];
  for (const line of text.split('\n')) {
    const kind = /^(Err|E\d{3}|.*:\d+:\d+: E\d{3})/.test(line) ? 'err' : /^(Warn|W\d{3})/.test(line) ? 'warn' : 'out';
    const last = out[out.length - 1];
    if (last && last.kind === kind) last.text += '\n' + line;
    else out.push({ kind, text: line });
  }
  return out;
}

// ---------------------------------------------------------------- socket

let backoff = 500;
function connect() {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  const ws = new WebSocket(`${proto}://${location.host}/api/ws`);
  ws.binaryType = 'arraybuffer';
  S.ws = ws;
  $('#conn').className = 'dot wait';
  ws.onopen = () => {
    backoff = 500;
    S.connected = true;
    $('#conn').className = 'dot on';
    if (S.wantListen) send({ t: 'listen', on: true });
  };
  ws.onclose = () => {
    S.connected = false;
    $('#conn').className = 'dot';
    for (const [, res] of S.waiting) res([]);
    S.waiting.clear();
    setTimeout(connect, backoff);
    backoff = Math.min(backoff * 2, 5000);
  };
  ws.onmessage = (e) => {
    if (typeof e.data !== 'string') {
      listener.feed(e.data);
      return;
    }
    let m;
    try {
      m = JSON.parse(e.data);
    } catch (_) {
      return;
    }
    onMessage(m);
  };
}

function send(m) {
  if (S.ws && S.ws.readyState === 1) {
    S.ws.send(JSON.stringify(m));
    return true;
  }
  return false;
}

function onMessage(m) {
  switch (m.t) {
    case 'hello':
      S.histKey = m.history_key;
      setPending(m.pending);
      break;
    case 'scrollback':
      T.replaceChildren();
      for (const ev of m.events) {
        if (ev.t === 'print') printEvent(ev);
      }
      scrollDown();
      break;
    case 'print':
      printEvent(m);
      break;
    case 'prompt':
      setPending(m.pending);
      break;
    case 'clear':
      T.replaceChildren();
      break;
    case 'history':
      if (m.key !== S.histKey) break;
      if (m.replace) S.history = m.entries;
      else S.history.push(...m.entries);
      S.histIdx = S.history.length;
      if (!$('[data-panel=history]').hidden) renderHistory();
      break;
    case 'completions': {
      const res = S.waiting.get(m.id);
      if (res) {
        S.waiting.delete(m.id);
        res(m.list);
      }
      break;
    }
    case 'status':
      onStatus(m.status || m);
      break;
    case 'engine':
      refreshEngine();
      break;
    case 'listen':
      if (m.error) {
        toast(m.error, true);
        stopListening();
      }
      break;
  }
}

function setPending(p) {
  S.pending = p;
  drawMarker();
}

function drawMarker() {
  $('#marker').textContent = MARKERS[markerIdx] + (S.pending ? '..' : '');
}
setInterval(() => {
  if (S.prefs.marker === false) return;
  markerIdx = (markerIdx + 1) % MARKERS.length;
  drawMarker();
}, 100);

function onStatus(st) {
  S.lastStatus = st;
  const parts = [];
  if (st.rate) parts.push(`${(st.rate / 1000).toFixed(st.rate % 1000 ? 1 : 0)}k`);
  if (st.xruns) parts.push(`xruns ${st.xruns}`);
  if (st.listeners) parts.push(`🎧${st.listeners}`);
  $('#stat').textContent = parts.join(' · ');
  $('#out-btn').textContent = st.output || '…';
  $('#out-btn').title = `output: ${st.output} (${st.output_id})`;
}

// ---------------------------------------------------------------- prompt

const input = $('#input');

function autosize() {
  input.style.height = 'auto';
  input.style.height = `${input.scrollHeight + 2}px`;
}

function submit() {
  const text = input.value;
  if (!S.connected) {
    toast('Not connected', true);
    return;
  }
  // a pasted block runs line by line, as in a terminal
  const lines = text.split('\n');
  for (const line of lines) send({ t: 'submit', line });
  input.value = '';
  S.histIdx = S.history.length;
  S.histSaved = '';
  S.cycle = null;
  hideChips();
  autosize();
  scrollDown();
}

function cancel() {
  const had = input.value;
  input.value = '';
  S.cycle = null;
  hideChips();
  autosize();
  send({ t: 'cancel' });
  addLine('local', `${MARKERS[markerIdx]}${S.pending ? '..' : ''} ${had}^C`);
}

function histPrev() {
  if (!S.history.length) return;
  if (S.histIdx >= S.history.length) S.histSaved = input.value;
  if (S.histIdx > 0) S.histIdx--;
  setInput(S.history[S.histIdx]);
}
function histNext() {
  if (S.histIdx >= S.history.length) return;
  S.histIdx++;
  setInput(S.histIdx === S.history.length ? S.histSaved : S.history[S.histIdx]);
}

function setInput(v, caret = v.length) {
  input.value = v;
  input.setSelectionRange(caret, caret);
  autosize();
}

function insertAtCursor(text) {
  const a = input.selectionStart;
  const b = input.selectionEnd;
  input.setRangeText(text, a, b, 'end');
  autosize();
  input.focus();
  S.cycle = null;
  scheduleLive();
}

function moveCursor(d) {
  const p = Math.max(0, Math.min(input.value.length, input.selectionStart + d));
  input.setSelectionRange(p, p);
}

// ---- completion (the terminal's Tab, plus tappable candidates)

function complete(line, cursorChars) {
  return new Promise((resolve) => {
    const id = ++S.reqId;
    S.waiting.set(id, resolve);
    if (!send({ t: 'complete', id, line, cursor: utf8len(line.slice(0, cursorChars)) })) {
      S.waiting.delete(id);
      resolve([]);
    }
    setTimeout(() => {
      if (S.waiting.delete(id)) resolve([]);
    }, 3000);
  });
}

const WORD = /[A-Za-z0-9_]/;
function wordEnd(s, i) {
  while (i < s.length && WORD.test(s[i])) i++;
  return i;
}
function commonPrefix(list) {
  let p = list[0] || '';
  for (const s of list) while (!s.startsWith(p)) p = p.slice(0, -1);
  return p;
}
const withoutDupEq = (c, eqAfter) => (eqAfter && c.endsWith('=') ? c.slice(0, -1) : c);

// where the candidates go: the stretch before the cursor they all start with
function contextFor(list) {
  const line = input.value;
  const cursor = input.selectionStart;
  const before = line.slice(0, cursor);
  let typed = 0;
  for (let n = before.length; n >= 0; n--) {
    const w = before.slice(before.length - n);
    if (list.every((c) => c.startsWith(w))) {
      typed = n;
      break;
    }
  }
  const start = cursor - typed;
  const end = Math.max(wordEnd(line, cursor), cursor);
  return { start, end, eqAfter: line[end] === '=' };
}

function replaceRange(start, end, text) {
  const v = input.value;
  input.value = v.slice(0, start) + text + v.slice(end);
  const c = start + text.length;
  input.setSelectionRange(c, c);
  autosize();
}

async function tab() {
  if (S.cycle) {
    const c = S.cycle;
    c.idx = c.idx == null ? 0 : (c.idx + 1) % c.list.length;
    const text = withoutDupEq(c.list[c.idx], c.eqAfter);
    replaceRange(c.start, c.start + c.len, text);
    c.len = text.length;
    markChip(c.idx);
    return;
  }
  const line = input.value;
  const list = await complete(line, input.selectionStart);
  if (input.value !== line || !list.length) return;
  const ctx = contextFor(list);
  if (list.length === 1) {
    replaceRange(ctx.start, ctx.end, withoutDupEq(list[0], ctx.eqAfter));
    hideChips();
    return;
  }
  const prefix = commonPrefix(list);
  if (prefix.length > ctx.end - ctx.start) {
    replaceRange(ctx.start, ctx.end, withoutDupEq(prefix, ctx.eqAfter));
    showChips(list, contextFor(list));
    return;
  }
  S.cycle = { list, idx: null, start: ctx.start, len: ctx.end - ctx.start, eqAfter: ctx.eqAfter };
  showChips(list, ctx);
}

function showChips(list, ctx) {
  const box = $('#chips');
  S.chipsCtx = ctx;
  box.replaceChildren(
    ...list.slice(0, 80).map((c, i) =>
      el('button', { type: 'button', onpointerdown: keepFocus, onclick: () => pickChip(c, i) }, c),
    ),
  );
  if (list.length > 80) box.append(el('span', { class: 'muted small' }, `… ${list.length - 80} more`));
  box.hidden = false;
}

function hideChips() {
  $('#chips').hidden = true;
  $('#chips').replaceChildren();
}

function markChip(i) {
  $$('#chips button').forEach((b, j) => b.classList.toggle('sel', i === j));
}

function pickChip(c) {
  // recompute: the line may have changed since the chips were drawn
  const ctx = contextFor([c]);
  const typedOk = input.value.slice(ctx.start, input.selectionStart);
  const start = c.startsWith(typedOk) ? ctx.start : input.selectionStart;
  replaceRange(start, Math.max(ctx.end, input.selectionStart), withoutDupEq(c, ctx.eqAfter));
  S.cycle = null;
  hideChips();
  input.focus();
  scheduleLive();
}

let liveTimer;
function scheduleLive() {
  clearTimeout(liveTimer);
  if (S.prefs.live_complete === false) return;
  liveTimer = setTimeout(async () => {
    const line = input.value;
    const cur = input.selectionStart;
    if (!line.trim() || document.activeElement !== input) {
      hideChips();
      return;
    }
    const list = await complete(line, cur);
    if (input.value !== line || input.selectionStart !== cur) return;
    const ctx = list.length ? contextFor(list) : null;
    const word = ctx ? line.slice(ctx.start, ctx.end) : '';
    const useful = list.filter((c) => c !== word && c !== word + '=');
    if (!useful.length) hideChips();
    else showChips(useful, ctx);
  }, 140);
}

input.addEventListener('input', () => {
  S.cycle = null;
  autosize();
  scheduleLive();
});

input.addEventListener('keydown', (e) => {
  const ctrl = e.ctrlKey || e.metaKey;
  if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    submit();
  } else if (e.key === 'Tab') {
    e.preventDefault();
    tab();
  } else if (e.key === 'ArrowUp' && !e.shiftKey && !input.value.slice(0, input.selectionStart).includes('\n')) {
    e.preventDefault();
    histPrev();
  } else if (e.key === 'ArrowDown' && !e.shiftKey && !input.value.slice(input.selectionEnd).includes('\n')) {
    e.preventDefault();
    histNext();
  } else if (ctrl && e.key === 'c' && input.selectionStart === input.selectionEnd) {
    e.preventDefault();
    cancel();
  } else if (ctrl && e.key === 'r') {
    e.preventDefault();
    openSheet('history');
    $('#hist-filter').value = input.value;
    renderHistory();
    $('#hist-filter').focus();
  } else if (e.key === 'Escape') {
    S.cycle = null;
    hideChips();
  } else if (e.key !== 'Shift') {
    if (S.cycle && e.key.length === 1) S.cycle = null;
  }
});

$('#prompt').addEventListener('submit', (e) => {
  e.preventDefault();
  submit();
});
$('#send').addEventListener('pointerdown', keepFocus);

function buildKeys() {
  const fn = [
    ['Tab', () => tab()],
    ['↑', histPrev],
    ['↓', histNext],
    ['←', () => moveCursor(-1)],
    ['→', () => moveCursor(1)],
    ['^C', cancel],
  ];
  const syms = S.prefs.symbol_keys || [];
  $('#keys').replaceChildren(
    ...fn.map(([label, f]) =>
      el('button', { type: 'button', class: 'fn', onpointerdown: keepFocus, onclick: () => { f(); input.focus(); } }, label),
    ),
    ...syms.map((s) =>
      el('button', { type: 'button', onpointerdown: keepFocus, onclick: () => insertAtCursor(s) }, s),
    ),
  );
}

// ---------------------------------------------------------------- listening

function startListening() {
  if (listener.active) return;
  S.wantListen = true;
  const rate = S.lastStatus?.rate || S.engine?.info?.rate || 48000;
  // AudioContext is made inside the tap, as iOS requires
  listener
    .start({ rate, jitterMs: S.prefs.jitter_ms ?? 120, volume: S.prefs.volume ?? 0.9 })
    .then(() => {
      send({ t: 'listen', on: true });
      setListenUi(true);
    })
    .catch((e) => {
      S.wantListen = false;
      toast(`Audio: ${e.message}`, true);
      listener.stop();
    });
}

function stopListening() {
  S.wantListen = false;
  send({ t: 'listen', on: false });
  listener.stop();
  setListenUi(false);
  $('#listen-stats').textContent = '';
}

function setListenUi(on) {
  S.listening = on;
  $('#listen').setAttribute('aria-pressed', on);
  $('#listen2').textContent = on ? 'Stop listening' : 'Start listening';
  $('#listen2').classList.toggle('primary', on);
}

listener.onstats = (s) => {
  $('#listen-stats').textContent =
    `${s.bufferedMs.toFixed(0)}/${s.targetMs.toFixed(0)} ms buffered · ${s.underruns} underruns · ${s.lost} lost · ${s.rate} Hz · ${s.kind}`;
};

const toggleListen = () => (listener.active ? stopListening() : startListening());
$('#listen').addEventListener('click', toggleListen);
$('#listen2').addEventListener('click', toggleListen);
$('#vol').addEventListener('input', (e) => listener.setVolume(+e.target.value));
$('#vol').addEventListener('change', (e) => savePrefs({ volume: +e.target.value }, true));
$('#jit').addEventListener('input', (e) => {
  $('#jit-v').textContent = `${e.target.value} ms`;
  listener.setJitter(+e.target.value);
});
$('#jit').addEventListener('change', (e) => savePrefs({ jitter_ms: +e.target.value }, true));

document.addEventListener(
  'pointerdown',
  () => {
    if (S.prefs.auto_listen && !listener.active && !S.wantListen) startListening();
  },
  { once: false, capture: true },
);

// ---------------------------------------------------------------- sheet

function openSheet(tab) {
  $('#sheet').hidden = false;
  showTab(tab);
}
function closeSheet() {
  $('#sheet').hidden = true;
}
function showTab(tab) {
  $$('.tab').forEach((b) => b.classList.toggle('on', b.dataset.tab === tab));
  $$('.panel').forEach((p) => {
    const on = p.dataset.panel === tab;
    p.classList.toggle('on', on);
    p.hidden = !on;
  });
  ({ audio: loadAudio, sources: loadSources, assets: loadAssets, history: renderHistory, help: loadHelp, settings: fillSettings })[tab]?.();
}
$$('.tab').forEach((b) => b.addEventListener('click', () => showTab(b.dataset.tab)));
$('#menu').addEventListener('click', () => ($('#sheet').hidden ? openSheet(localStorage.getItem('tab') || 'audio') : closeSheet()));
$('#sheet-close').addEventListener('click', closeSheet);
$('#out-btn').addEventListener('click', () => openSheet('audio'));
$('#tabs').addEventListener('click', (e) => {
  if (e.target.dataset.tab) {
    try {
      localStorage.setItem('tab', e.target.dataset.tab);
    } catch (_) {}
  }
});

function runLines(lines, close = true) {
  for (const line of lines) if (!send({ t: 'submit', line })) return toast('Not connected', true);
  if (close && innerWidth < 900) closeSheet();
}

function run(line, close = true) {
  if (!send({ t: 'submit', line })) {
    toast('Not connected', true);
    return;
  }
  if (close && innerWidth < 900) closeSheet();
}

// ---- audio panel

async function refreshEngine() {
  try {
    S.engine = await api('GET', '/api/engine');
    const i = S.engine.info;
    $('#engine-info').textContent =
      `session #${i.generation} · ${i.rate} Hz · ${i.channels} ch\nassets: ${i.assets_dir}\nsources: ${i.sources_dir}`;
    $('#out-btn').textContent = i.output.name;
    if (!$('#sheet').hidden && $('[data-panel=audio]').classList.contains('on')) renderCurrent();
  } catch (_) {}
}

function renderCurrent() {
  const o = S.engine?.info?.output;
  if (!o) return;
  $('#out-current').replaceChildren(
    el('div', { class: 't mono' }, o.name),
    el('div', { class: 'muted small mono' }, `${o.id} · ${o.rate} Hz · ${o.channels} ch · ${o.format} · buffer ${o.buffer}`),
  );
}

async function loadAudio() {
  await refreshEngine();
  renderCurrent();
  loadDevices();
  loadPipewire();
}

async function loadDevices() {
  const ul = $('#devices');
  ul.replaceChildren(el('li', { class: 'muted' }, 'probing devices…'));
  try {
    const d = await api('GET', `/api/devices?all=${$('#dev-all').checked}`);
    const cur = d.current.id;
    const item = (id, name, sub, ok = true) =>
      el(
        'li',
        { class: `${id === cur ? 'cur' : ''} ${ok ? '' : 'off'}` },
        el('div', { class: 'main', onclick: () => setOutput(id, name, ok) },
          el('div', { class: 't' }, name),
          el('div', { class: 's mono' }, sub),
        ),
      );
    ul.replaceChildren(
      item('none', 'No local output (browser only)', 'software clock · listen in the browser'),
      ...d.devices.map((x) =>
        item(x.id, x.name + (x.is_default ? ' (default)' : ''), `${x.id} · ${x.summary}${x.rate_ok ? '' : ` · not at ${d.rate} Hz`}${x.direct ? ' · direct (bypasses the sound server)' : ''}`, x.rate_ok),
      ),
    );
  } catch (e) {
    ul.replaceChildren(el('li', { class: 'muted' }, e.message));
  }
}

async function setOutput(id, name, rateOk) {
  try {
    toast(`Switching to ${name}…`);
    await api('POST', '/api/output', { id, restart: false });
  } catch (e) {
    if (/can't run at/.test(e.message) || !rateOk) {
      if (confirm(`${e.message}\n\nRestart the engine at the device's rate? The session is kept (as its state script); playback restarts.`)) {
        try {
          await api('POST', '/api/output', { id, restart: true });
        } catch (e2) {
          toast(e2.message, true);
        }
      }
    } else toast(e.message, true);
  }
  loadAudio();
}

async function loadPipewire() {
  if (!S.me?.pipewire_routing) return;
  try {
    const r = await api('GET', '/api/pipewire');
    $('#pw-wrap').hidden = !r.available;
    $('#pw-note').textContent = r.note || '';
    $('#pw-sinks').replaceChildren(
      ...r.sinks.map((s) =>
        el(
          'li',
          { class: s.current ? 'cur' : '' },
          el('div', { class: 'main', onclick: () => route(s) },
            el('div', { class: 't' }, s.description),
            el('div', { class: 's mono' }, s.name),
          ),
        ),
      ),
    );
  } catch (_) {
    $('#pw-wrap').hidden = true;
  }
}

async function route(sink) {
  try {
    await api('POST', '/api/pipewire/route', { sink: sink.name });
    toast(`Routed to ${sink.description}`);
  } catch (e) {
    toast(e.message, true);
  }
  loadPipewire();
}

$('#dev-refresh').addEventListener('click', () => {
  loadDevices();
  loadPipewire();
});
$('#dev-all').addEventListener('change', loadDevices);
$('#restart-keep').addEventListener('click', async () => {
  try {
    await api('POST', '/api/engine/restart', { keep_state: true });
  } catch (e) {
    toast(e.message, true);
  }
});
$('#restart-fresh').addEventListener('click', async () => {
  if (!confirm('Restart with an empty session? Everything you built is dropped (save it with "state to NAME" first).')) return;
  try {
    await api('POST', '/api/engine/restart', { keep_state: false });
  } catch (e) {
    toast(e.message, true);
  }
});

// ---- sources panel

const ident = (s) => /^[a-z][a-z0-9_]*$/.test(s);
function sourceCmd(name, suffix = '') {
  const target = ident(name) ? name : JSON.stringify(`${S.sourcesDir}/${name}.blast`);
  return `source ${target}${suffix}`;
}

async function loadSources() {
  const ul = $('#sources');
  try {
    const r = await api('GET', '/api/sources');
    S.sourcesDir = r.dir;
    ul.replaceChildren(
      ...r.sources.map((s) =>
        el(
          'li',
          {},
          el('div', { class: 'main', onclick: () => openSource(s.name) },
            el('div', { class: 't' }, s.name),
            el('div', { class: 's' }, `${(s.size / 1024).toFixed(1)} KB · ${new Date(s.modified * 1000).toLocaleString()}`),
          ),
          el('button', { class: 'btn small', onclick: () => run(sourceCmd(s.name)) }, '▶ run'),
        ),
      ),
    );
    if (!r.sources.length) ul.replaceChildren(el('li', { class: 'muted' }, `no .blast files in ${r.dir}`));
  } catch (e) {
    ul.replaceChildren(el('li', { class: 'muted' }, e.message));
  }
}

const ed = { name: null };
async function openSource(name) {
  try {
    const text = name ? await api('GET', `/api/sources/${encodeURI(name)}`) : '';
    ed.name = name;
    $('#ed-name').value = name || '';
    $('#ed-text').value = text;
    $('#ed-msg').textContent = '';
    $('#editor').hidden = false;
    $('#sources').hidden = true;
  } catch (e) {
    toast(e.message, true);
  }
}

function closeEditor() {
  $('#editor').hidden = true;
  $('#sources').hidden = false;
  loadSources();
}

async function saveSource() {
  const name = $('#ed-name').value.trim().replace(/\.blast$/, '');
  if (!name) {
    toast('Name it first', true);
    return null;
  }
  const overwrite = ed.name === name;
  try {
    await api('PUT', `/api/sources/${encodeURI(name)}?overwrite=${overwrite ? 1 : 0}`, $('#ed-text').value, true);
  } catch (e) {
    if (e.status === 409 && confirm(`${name}.blast exists. Overwrite it?`)) {
      await api('PUT', `/api/sources/${encodeURI(name)}?overwrite=1`, $('#ed-text').value, true);
    } else {
      toast(e.message, true);
      return null;
    }
  }
  ed.name = name;
  edMsg(`saved ${name}.blast`, 'ok');
  return name;
}

function edMsg(text, cls = '') {
  const m = $('#ed-msg');
  m.textContent = text;
  m.className = `small ${cls}`;
}

$('#src-new').addEventListener('click', () => openSource(null));
$('#src-refresh').addEventListener('click', loadSources);
$('#ed-close').addEventListener('click', closeEditor);
$('#ed-save').addEventListener('click', saveSource);
$('#ed-check').addEventListener('click', async () => {
  const name = $('#ed-name').value.trim() || 'editor';
  const r = await api('POST', '/api/check', { name, text: $('#ed-text').value });
  if (r.ok) edMsg(['ok', ...r.warnings].join('\n'), 'ok');
  else edMsg([...r.errors, ...r.warnings].join('\n'), 'err');
});
$('#ed-run').addEventListener('click', async () => {
  const name = await saveSource();
  if (name) run(sourceCmd(name));
});
$('#ed-hist').addEventListener('click', async () => {
  const name = await saveSource();
  if (name) run(sourceCmd(name, ' history'), false);
});
$('#ed-sel').addEventListener('click', () => {
  const ta = $('#ed-text');
  const v = ta.value;
  let a = ta.selectionStart;
  let b = ta.selectionEnd;
  // the whole lines the selection touches (or the cursor's line)
  a = v.lastIndexOf('\n', a - 1) + 1;
  const nl = v.indexOf('\n', b === a ? b : b - 1);
  b = nl < 0 ? v.length : nl;
  const lines = v.slice(a, b).split('\n');
  for (const line of lines) send({ t: 'submit', line });
  edMsg(`ran ${lines.length} line${lines.length === 1 ? '' : 's'} at the prompt`, 'ok');
});
$('#src-state').addEventListener('click', () => {
  const name = prompt('Save the session as (sources/NAME.blast):', 'snapshot');
  if (name) {
    if (!ident(name)) toast('Names are lowercase letters, digits and _', true);
    else {
      run(`state to ${name}`, false);
      setTimeout(loadSources, 500);
    }
  }
});
$('#src-upload').addEventListener('change', async (e) => {
  for (const f of e.target.files) {
    const name = f.name.replace(/\.blast$/, '').replace(/[^A-Za-z0-9_\-./]/g, '_');
    const text = await f.text();
    try {
      await api('PUT', `/api/sources/${encodeURI(name)}`, text, true);
    } catch (err) {
      if (err.status === 409 && confirm(`${name}.blast exists. Overwrite it?`)) {
        await api('PUT', `/api/sources/${encodeURI(name)}?overwrite=1`, text, true);
      } else toast(err.message, true);
    }
  }
  e.target.value = '';
  loadSources();
});

// ---- assets panel

let assets = [];
async function loadAssets() {
  $('#assets').replaceChildren(el('li', { class: 'muted' }, 'scanning…'));
  try {
    const r = await api('GET', '/api/assets');
    assets = r.assets;
    $('#as-filter').placeholder = `filter ${r.assets.length} files in ${r.dir}`;
    renderAssets();
  } catch (e) {
    $('#assets').replaceChildren(el('li', { class: 'muted' }, e.message));
  }
}

function voiceName(track) {
  let n = track.split('/').pop().toLowerCase().replace(/[^a-z0-9_]/g, '_');
  if (!/^[a-z]/.test(n)) n = 'v' + n;
  return n;
}

const fmtSize = (b) => (b > 1048576 ? `${(b / 1048576).toFixed(1)} MB` : `${(b / 1024).toFixed(0)} KB`);

function renderAssets() {
  const q = $('#as-filter').value.toLowerCase();
  const ul = $('#assets');
  const items = [];
  let dir = null;
  for (const a of assets) {
    if (q && !a.path.toLowerCase().includes(q)) continue;
    const d = a.path.includes('/') ? a.path.slice(0, a.path.lastIndexOf('/')) : '';
    if (d !== dir) {
      dir = d;
      if (d) items.push(el('li', { class: 'dir' }, `${d}/`));
    }
    const track = a.track || a.path.replace(/\.[^.]+$/, '');
    const badge = !a.playable
      ? el('span', { class: 'badge' }, 'unreadable')
      : a.loaded
        ? el('span', { class: 'badge ok' }, 'loaded')
        : el('span', { class: 'badge' }, 'new: rescan');
    items.push(
      el(
        'li',
        { class: a.playable ? '' : 'off' },
        el('button', { class: 'btn small', title: 'preview here', onclick: (e) => preview(a, e.currentTarget) }, '▶'),
        el('div', { class: 'main', title: 'insert the track name at the prompt', onclick: () => { insertAtCursor(JSON.stringify(track)); if (innerWidth < 900) closeSheet(); } },
          el('div', { class: 't' }, track),
          el('div', { class: 's' }, [a.rate ? `${a.rate} Hz` : null, a.channels ? `${a.channels} ch` : null, fmtSize(a.size)].filter(Boolean).join(' · ')),
        ),
        badge,
        el('button', {
          class: 'btn small', title: 'voice NAME from "track"', disabled: !a.loaded,
          onclick: () => { setInput(`voice ${voiceName(track)} from ${JSON.stringify(track)}`); if (innerWidth < 900) closeSheet(); input.focus(); },
        }, '+voice'),
      ),
    );
  }
  ul.replaceChildren(...items);
  if (!items.length) ul.append(el('li', { class: 'muted' }, 'nothing here'));
}

let previewBtn = null;
function preview(a, btn) {
  const audio = $('#preview');
  if (previewBtn === btn && !audio.paused) {
    audio.pause();
    btn.textContent = '▶';
    previewBtn = null;
    return;
  }
  if (previewBtn) previewBtn.textContent = '▶';
  previewBtn = btn;
  btn.textContent = '■';
  audio.src = `/api/assets/preview?path=${encodeURIComponent(a.path)}`;
  audio.play().catch((e) => toast(`Preview: ${e.message}`, true));
  audio.onended = () => {
    btn.textContent = '▶';
    previewBtn = null;
  };
}

$('#as-filter').addEventListener('input', renderAssets);
$('#as-refresh').addEventListener('click', loadAssets);
$('#as-rescan').addEventListener('click', () => {
  run('rescan', false);
  setTimeout(loadAssets, 700);
});
$('#as-upload').addEventListener('change', async (e) => {
  const files = [...e.target.files];
  const folder = $('#as-folder').value.trim().replace(/^\/+|\/+$/g, '');
  let done = 0;
  for (const f of files) {
    const path = folder ? `${folder}/${f.name}` : f.name;
    $('#as-progress').textContent = `uploading ${f.name} (${done + 1}/${files.length})…`;
    const url = (ow) => `/api/assets/upload?path=${encodeURIComponent(path)}&overwrite=${ow ? 1 : 0}`;
    try {
      await api('PUT', url(false), f, true);
      done++;
    } catch (err) {
      if (err.status === 409 && confirm(`${path} exists. Replace it?`)) {
        await api('PUT', url(true), f, true).then(() => done++, (e2) => toast(e2.message, true));
      } else toast(`${f.name}: ${err.message}`, true);
    }
  }
  $('#as-progress').textContent = `${done} of ${files.length} uploaded`;
  e.target.value = '';
  if (done && S.prefs.rescan_after_upload !== false) {
    run('rescan', false);
    setTimeout(loadAssets, 700);
  } else loadAssets();
});

// ---- history panel

function renderHistory() {
  const q = $('#hist-filter').value.toLowerCase();
  const items = [];
  for (let i = S.history.length - 1; i >= 0 && items.length < 500; i--) {
    const h = S.history[i];
    if (q && !h.toLowerCase().includes(q)) continue;
    items.push(
      el(
        'li',
        {},
        el('div', { class: 'main', onclick: () => { S.histIdx = i; setInput(h); closeSheet(); input.focus(); } },
          el('div', { class: 't' }, h.replace(/\n/g, ' ⏎ ')),
        ),
        el('button', { class: 'btn small', title: 'run it', onclick: () => runLines(h.split('\n')) }, '▶'),
      ),
    );
  }
  $('#history').replaceChildren(...items);
  if (!items.length) $('#history').append(el('li', { class: 'muted' }, S.history.length ? 'no match' : 'no history yet'));
}
$('#hist-filter').addEventListener('input', renderHistory);
$('#hist-filter').addEventListener('keydown', (e) => {
  if (e.key === 'Enter') {
    const first = $('#history .main');
    if (first) first.click();
  } else if (e.key === 'Escape') {
    closeSheet();
    input.focus();
  }
});

// ---- help panel

let cardLoaded = false;
async function loadHelp() {
  const cmds = ['help', 'help voice', 'help seq', 'help lfo', 'help env', 'help filter', 'help buffer', 'help source', 'state'];
  $('#help-buttons').replaceChildren(...cmds.map((c) => el('button', { class: 'btn small mono', onclick: () => run(c) }, c)));
  if (cardLoaded) return;
  try {
    $('#card').textContent = await api('GET', '/api/card');
    cardLoaded = true;
  } catch (e) {
    $('#card').textContent = e.message;
  }
}

// ---- settings panel

function fillSettings() {
  const p = S.prefs;
  $('#p-theme').value = p.theme || 'auto';
  $('#p-font').value = p.font_size || 14;
  $('#p-live').checked = p.live_complete !== false;
  $('#p-marker').checked = p.marker !== false;
  $('#p-auto').checked = !!p.auto_listen;
  $('#p-rescan').checked = p.rescan_after_upload !== false;
  $('#p-keys').value = (p.symbol_keys || []).join(' ');
  if (S.me) $('#who').textContent = `Signed in as ${S.me.name} (${S.me.login})${S.me.config_path ? ` · config ${S.me.config_path}` : ''}`;
}
$('#p-save').addEventListener('click', () =>
  savePrefs({
    theme: $('#p-theme').value,
    font_size: +$('#p-font').value,
    live_complete: $('#p-live').checked,
    marker: $('#p-marker').checked,
    auto_listen: $('#p-auto').checked,
    rescan_after_upload: $('#p-rescan').checked,
    symbol_keys: $('#p-keys').value.split(/\s+/).filter(Boolean),
  }),
);
$('#p-reset').addEventListener('click', async () => {
  S.prefs = await api('DELETE', '/api/prefs');
  applyPrefs();
  toast('Back to defaults');
});

// ---------------------------------------------------------------- viewport

// iOS: keep the prompt above the on-screen keyboard
function fitViewport() {
  const vv = window.visualViewport;
  const h = vv ? vv.height : innerHeight;
  document.documentElement.style.setProperty('--vh', `${h}px`);
  if (vv) window.scrollTo(0, 0);
}
window.visualViewport?.addEventListener('resize', () => {
  const stick = nearBottom();
  fitViewport();
  if (stick) scrollDown();
});
window.visualViewport?.addEventListener('scroll', fitViewport);
fitViewport();

// ---------------------------------------------------------------- boot

(async function boot() {
  try {
    S.me = await api('GET', '/api/me');
    S.prefs = S.me.prefs;
    S.histKey = S.me.history_key;
  } catch (e) {
    addLine('err', `Could not load the session: ${e.message}`);
  }
  applyPrefs();
  connect();
  refreshEngine();
  // deep links: /#audio, /#sources, /#assets, /#history, /#help, /#settings
  const tab = location.hash.slice(1);
  if ($(`[data-panel="${tab}"]`)) openSheet(tab);
  else if (matchMedia('(pointer: fine)').matches) input.focus();
})();
