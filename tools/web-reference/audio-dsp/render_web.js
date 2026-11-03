// Paste into the lab player page (/new.html). Needs the local receiver on 127.0.0.1:5199 (writes POST bodies to files).
const T = window.TVFSPlayer; const url = T.audio.currentSrc || T.audio.src;
const data = await (await fetch(url)).arrayBuffer();
const buf = await new OfflineAudioContext(2, 1, 44100).decodeAudioData(data);
const sr = buf.sampleRate, start = 40, dur = 6, len = Math.round(dur * sr);
const RATE = ['tempo', 'pitch', 'nightcore'];
async function render(settings, eq) {
  const oc = new OfflineAudioContext({ numberOfChannels: 2, length: len, sampleRate: sr });
  const src = oc.createBufferSource(); src.buffer = buf;
  const E = T.createEngine(oc, src); await E.loadWorklet();
  for (const id of Object.keys(T.controls)) if (!RATE.includes(id)) E.set(id, id in settings ? settings[id] : T.controls[id].def, true);
  if (settings.pitch) E.setPitchShift(Math.pow(2, settings.pitch / 12));
  if (eq) E.setEq(eq, true);
  src.start(0, start);
  const out = await oc.startRendering();
  const L = out.getChannelData(0), R = out.getChannelData(1), il = new Float32Array(len * 2);
  for (let i = 0; i < len; i++) { il[2 * i] = L[i]; il[2 * i + 1] = R[i]; }
  return il;
}
const flat = [60, 150, 400, 1000, 2500, 6000, 12000].map((f, i) => ({ type: i == 0 ? 'lowshelf' : i == 6 ? 'highshelf' : 'peaking', freq: f, gain: 0, Q: 1 }));
const eqBoost = flat.map((b, i) => i == 3 ? Object.assign({}, b, { gain: 6 }) : b);
const eqShelf = flat.map((b, i) => i == 0 ? Object.assign({}, b, { gain: 6, freq: 90 }) : b);
const cases = { neutral: [{}], eq_1k_plus6: [{}, eqBoost], eq_lowshelf90_plus6: [{}, eqShelf], filter_m40: [{ filter: -40 }], filter_p40: [{ filter: 40 }],
  width0: [{ width: 0 }], width180: [{ width: 180 }], rotate50: [{ rotate: 50 }], volume150: [{ volume: 150 }], volume50: [{ volume: 50 }],
  pitch7: [{ pitch: 7 }], reverb40: [{ reverb: 40, room: 70 }], echo40: [{ echo: 40 }] };
const dry = new Float32Array(len * 2); const dl = buf.getChannelData(0), dr = buf.getChannelData(1), s0 = start * sr;
for (let i = 0; i < len; i++) { dry[2 * i] = dl[s0 + i]; dry[2 * i + 1] = dr[s0 + i]; }
await fetch('http://127.0.0.1:5199/web_input.f32', { method: 'POST', mode: 'no-cors', body: dry.buffer });
for (const [name, [s, eq]] of Object.entries(cases)) {
  const r = await render(s, eq);
  await fetch('http://127.0.0.1:5199/web_' + name + '.f32', { method: 'POST', mode: 'no-cors', body: r.buffer });
}
