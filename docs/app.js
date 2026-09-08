/* isodapane.ch — how far your money goes on ZVV public transport.
 *
 * The multi-label search here is a port of crates/build/src/search.rs, with one
 * addition the build side does not need: labels keep a pointer to the label they
 * came from, so the winning route to any stop can be walked back and shown. That
 * costs one integer per label and changes no fare.
 *
 * `validate --dump` diffs this against the Rust results over every station pair.
 */

const DATA = 'data/';
const MAP_CENTRE = [8.65, 47.40];

/* ------------------------------------------------------------------- state */
const state = {
  zones: null,
  stations: null,
  graph: null,
  fares: null,
  origin: null,        // station index
  result: null,        // last search result
  zoneFare: new Map(), // zone_id -> { weight, price } | null
  budget: 10.0,
  budgetWanted: 10.0,  // what the user last asked for, before any clamp
  klass: 'second',
  reduced: false,
  hovered: null,       // station index under the cursor
};

/* ------------------------------------------------------------------ search
 * Zone sets are u64 bitmasks in Rust. JavaScript's bitwise operators coerce to
 * int32, so a 45-bit mask would silently truncate under `|`. Every set is
 * therefore carried as two 32-bit halves and operated on pairwise, keeping the
 * hot loop on fast integer ops rather than BigInt.
 */
const LABEL_CAP = 64;

function search(graph, source) {
  const n = graph.adjacency.length;
  const { zoneWeights, zoneBits, adjacency, maskLo, maskHi } = graph;

  // Label records, append-only so that parent pointers stay valid even after a
  // label is pruned off its node's frontier.
  const lLo = [], lHi = [], lHops = [], lPrev = [], lNode = [];
  const frontier = new Array(n).fill(null);

  const addLabel = (lo, hi, hops, prev, node) => {
    lLo.push(lo); lHi.push(hi); lHops.push(hops); lPrev.push(prev); lNode.push(node);
    return lLo.length - 1;
  };

  const weightOf = (lo, hi) => {
    let w = 0;
    let x = lo;
    while (x !== 0) { const b = 31 - Math.clz32(x & -x); w += zoneWeights[b]; x &= x - 1; }
    x = hi;
    while (x !== 0) { const b = 31 - Math.clz32(x & -x); w += zoneWeights[b + 32]; x &= x - 1; }
    return w;
  };

  const bit = zoneBits[source];
  const startLo = bit < 32 ? (1 << bit) >>> 0 : 0;
  const startHi = bit >= 32 ? (1 << (bit - 32)) >>> 0 : 0;
  const start = addLabel(startLo, startHi, 1, -1, source);
  frontier[source] = [start];

  // Binary heap ordered by (zone weight, hops), as in search.rs.
  const heap = [{ w: weightOf(startLo, startHi), h: 1, li: start }];
  const less = (a, b) => (a.w !== b.w ? a.w < b.w : a.h < b.h);
  const push = (e) => {
    heap.push(e);
    let i = heap.length - 1;
    while (i > 0) {
      const p = (i - 1) >> 1;
      if (!less(heap[i], heap[p])) break;
      [heap[i], heap[p]] = [heap[p], heap[i]];
      i = p;
    }
  };
  const pop = () => {
    const top = heap[0];
    const last = heap.pop();
    if (heap.length) {
      heap[0] = last;
      let i = 0;
      for (;;) {
        const l = 2 * i + 1, r = l + 1;
        let m = i;
        if (l < heap.length && less(heap[l], heap[m])) m = l;
        if (r < heap.length && less(heap[r], heap[m])) m = r;
        if (m === i) break;
        [heap[i], heap[m]] = [heap[m], heap[i]];
        i = m;
      }
    }
    return top;
  };

  while (heap.length) {
    const e = pop();
    const u = lNode[e.li];
    // Skip labels pruned off the frontier since being queued.
    if (frontier[u].indexOf(e.li) < 0) continue;

    const eLo = lLo[e.li], eHi = lHi[e.li], eHops = lHops[e.li];
    const ns = adjacency[u], ml = maskLo[u], mh = maskHi[u];

    for (let k = 0; k < ns.length; k++) {
      const v = ns[k];
      const cLo = (eLo | ml[k]) >>> 0;
      const cHi = (eHi | mh[k]) >>> 0;

      let f = frontier[v];
      if (f === null) f = frontier[v] = [];

      // Dominated by something already here? (subset => no worse future)
      let dominated = false;
      for (let i = 0; i < f.length; i++) {
        const j = f[i];
        if ((lLo[j] & ~cLo) === 0 && (lHi[j] & ~cHi) === 0) { dominated = true; break; }
      }
      if (dominated) continue;

      // Drop whatever the candidate now dominates.
      let w = 0;
      for (let i = 0; i < f.length; i++) {
        const j = f[i];
        if (!((cLo & ~lLo[j]) === 0 && (cHi & ~lHi[j]) === 0)) f[w++] = j;
      }
      f.length = w;

      const li = addLabel(cLo, cHi, eHops + 1, e.li, v);
      f.push(li);

      if (f.length > LABEL_CAP) {
        f.sort((a, b) => weightOf(lLo[a], lHi[a]) - weightOf(lLo[b], lHi[b]));
        f.length = LABEL_CAP;
        if (f.indexOf(li) < 0) continue;
      }

      push({ w: weightOf(cLo, cHi), h: eHops + 1, li });
    }
  }

  // Best label per station, and the fare that follows from it.
  const weights = new Int32Array(n).fill(-1);
  const hops = new Int32Array(n).fill(-1);
  const best = new Int32Array(n).fill(-1);
  for (let v = 0; v < n; v++) {
    const f = frontier[v];
    if (!f || !f.length) continue;
    let bi = -1, bw = Infinity;
    for (let i = 0; i < f.length; i++) {
      const w = weightOf(lLo[f[i]], lHi[f[i]]);
      if (w < bw) { bw = w; bi = f[i]; }
    }
    weights[v] = bw;
    hops[v] = lHops[bi] - 1;   // hops counts the origin; stops travelled is one less
    best[v] = bi;
  }

  /* The stop-by-stop route to `v`, as station indices from origin to `v`. */
  const pathTo = (v) => {
    if (best[v] < 0) return null;
    const out = [];
    for (let li = best[v]; li >= 0; li = lPrev[li]) out.push(lNode[li]);
    return out.reverse();
  };

  return { weights, hops, pathTo };
}

/* ------------------------------------------------------------------- fares */
function priceFor(weight) {
  if (weight < 1) return null;
  const stufe = Math.min(Math.max(weight, 1), state.fares.max_stufe);
  const row = state.fares.stufen.find((s) => s.stufe === stufe);
  if (!row) return null;
  const key = state.reduced
    ? (state.klass === 'first' ? 'reduced_first' : 'reduced_second')
    : (state.klass === 'first' ? 'first' : 'second');
  return row[key];
}

/* Aggregate station weights to zones: a zone shows the cheapest fare among the
 * stations inside it, which keeps the polygon look while the search stays at
 * station level. */
function aggregateToZones(weights) {
  const best = new Map();
  const st = state.stations;
  for (let i = 0; i < st.length; i++) {
    const w = weights[i];
    if (w < 0) continue;
    const z = st[i].zone_id;
    const cur = best.get(z);
    if (cur === undefined || w < cur) best.set(z, w);
  }
  state.zoneFare = new Map();
  for (const [z, w] of best) state.zoneFare.set(z, { weight: w, price: priceFor(w) });
}

/* The slider should reach the priciest place actually reachable, and no further
 * — the ceiling moves when the class or reduction column changes. */
function updateBudgetRange() {
  let max = 0;
  for (const info of state.zoneFare.values()) {
    if (info.price !== null && info.price > max) max = info.price;
  }
  if (max <= 0) max = 20;
  const slider = document.getElementById('budget');
  slider.max = max.toFixed(2);
  // Clamp to the ceiling for display, but remember what was asked for, so that
  // switching to the reduced column and back does not quietly shrink the budget.
  state.budget = Math.min(state.budgetWanted, max);
  slider.value = state.budget;
  document.getElementById('budget-value').textContent = `CHF ${state.budget.toFixed(2)}`;
  document.getElementById('budget-max').textContent = `max CHF ${max.toFixed(2)}`;
}

/* ------------------------------------------------------------------ colour
 * Fare is a step function with hard edges. Colouring it as a gradient would
 * invent a continuity the tariff does not have, so each Tarifstufe gets a flat
 * band. */
const BAND_COLOURS = [
  '#1a9850', '#66bd63', '#a6d96a', '#fee08b',
  '#fdae61', '#f46d43', '#d73027', '#a50026',
];
const colourForStufe = (s) => BAND_COLOURS[Math.min(Math.max(s, 1), 8) - 1];

/* --------------------------------------------------------------- rendering */
let map;

function paintZones() {
  const affordable = [];
  const colours = ['case'];

  for (const f of state.zones.features) {
    const id = f.properties.zone_id;
    const info = state.zoneFare.get(id);
    if (!info || info.price === null) continue;
    if (info.price <= state.budget + 1e-9) affordable.push(id);
    colours.push(['==', ['get', 'zone_id'], id], colourForStufe(info.weight));
  }
  colours.push('#cccccc');

  map.setPaintProperty('zone-fill', 'fill-color', colours);
  map.setPaintProperty('zone-fill', 'fill-opacity', [
    'case',
    ['in', ['get', 'zone_id'], ['literal', affordable]], 0.62,
    0.05,
  ]);
  renderLegend();
}

function renderLegend() {
  const seen = new Map();
  for (const [, info] of state.zoneFare) {
    if (info.price === null) continue;
    const stufe = Math.min(Math.max(info.weight, 1), 8);
    if (!seen.has(stufe)) seen.set(stufe, info.price);
  }
  const rows = [...seen.entries()].sort((a, b) => a[0] - b[0]);
  const el = document.getElementById('legend-items');
  if (!rows.length) { el.innerHTML = '<div class="hint">Click the map to pick a starting point.</div>'; return; }
  el.innerHTML = rows.map(([stufe, price]) => {
    const label = state.fares.stufen.find((s) => s.stufe === stufe)?.label ?? `${stufe} Zonen`;
    const off = price > state.budget + 1e-9 ? ' over' : '';
    return `<div class="legend-row${off}">
      <span class="swatch" style="background:${colourForStufe(stufe)}"></span>
      <span class="lg-label">${label}</span>
      <span class="lg-price">CHF ${price.toFixed(2)}</span>
    </div>`;
  }).join('');
}

/* Nearest station to a point, by planar distance in degrees corrected for
 * latitude — exact enough over the canton. */
function nearestStation(lng, lat) {
  const k = Math.cos((lat * Math.PI) / 180);
  let best = -1, bestD = Infinity;
  const st = state.stations;
  for (let i = 0; i < st.length; i++) {
    const dx = (st[i].lon - lng) * k;
    const dy = st[i].lat - lat;
    const d = dx * dx + dy * dy;
    if (d < bestD) { bestD = d; best = i; }
  }
  return best;
}

/* The search result is kept so that switching class or reduction only reselects
 * a price column instead of re-running the search. */
function setOrigin(idx) {
  state.origin = idx;
  const t0 = performance.now();
  state.result = search(state.graph, idx);
  const ms = performance.now() - t0;

  aggregateToZones(state.result.weights);
  updateBudgetRange();
  paintZones();

  const s = state.stations[idx];
  map.getSource('origin').setData({
    type: 'Feature',
    geometry: { type: 'Point', coordinates: [s.lon, s.lat] },
  });
  document.getElementById('origin-name').textContent = s.name;
  document.getElementById('origin-zone').textContent = `Zone ${s.zone_id}`;
  document.getElementById('timing').textContent = `${ms.toFixed(0)} ms`;
  clearHover();
}

/* Reprice without re-running the search. */
function repriceOnly() {
  if (state.result === null) return;
  aggregateToZones(state.result.weights);
  updateBudgetRange();
  paintZones();
}

/* -------------------------------------------------------------- hover trace */
const escapeHtml = (s) => s.replace(/[&<>"]/g, (c) =>
  ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

/* A long route is unreadable in full, so show both ends and elide the middle. */
function formatTrace(path) {
  const names = path.map((i) => escapeHtml(state.stations[i].name));
  if (names.length <= 9) return names.join(' → ');
  const head = names.slice(0, 4).join(' → ');
  const tail = names.slice(-4).join(' → ');
  return `${head} → <span class="elided">… ${names.length - 8} more …</span> → ${tail}`;
}

function clearHover() {
  if (state.hovered !== null) {
    map.setFeatureState({ source: 'stations', id: state.hovered }, { hover: false });
    state.hovered = null;
  }
  map.getSource('route').setData({ type: 'FeatureCollection', features: [] });
}

function showHover(lngLat, popup) {
  const idx = nearestStation(lngLat.lng, lngLat.lat);

  if (idx !== state.hovered) {
    if (state.hovered !== null) {
      map.setFeatureState({ source: 'stations', id: state.hovered }, { hover: false });
    }
    state.hovered = idx;
    map.setFeatureState({ source: 'stations', id: idx }, { hover: true });

    // Draw the winning route to this stop.
    const path = state.result ? state.result.pathTo(idx) : null;
    map.getSource('route').setData(
      path && path.length > 1
        ? {
            type: 'Feature',
            geometry: {
              type: 'LineString',
              coordinates: path.map((i) => [state.stations[i].lon, state.stations[i].lat]),
            },
          }
        : { type: 'FeatureCollection', features: [] },
    );
  }

  const s = state.stations[idx];
  let html = `<div class="pop-stop">${escapeHtml(s.name)}</div>
              <div class="pop-zone">Zone ${s.zone_id}</div>`;

  if (state.result) {
    const w = state.result.weights[idx];
    if (w < 0) {
      html += '<div class="pop-none">not reachable on this network</div>';
    } else {
      const price = priceFor(w);
      const hops = state.result.hops[idx];
      const path = state.result.pathTo(idx);
      html += `<div class="pop-fare"><b>CHF ${price.toFixed(2)}</b>
                 · ${w} zone${w === 1 ? '' : 's'} billed</div>
               <div class="pop-hops">${hops} stop${hops === 1 ? '' : 's'} from
                 ${escapeHtml(state.stations[state.origin].name)}</div>`;
      if (path && path.length > 1) {
        html += `<div class="pop-trace">${formatTrace(path)}</div>`;
      }
    }
  }
  popup.setLngLat(lngLat).setHTML(html).addTo(map);
}

/* ------------------------------------------------------------------- boot */
async function main() {
  const [zones, stations, graphRaw, fares] = await Promise.all(
    ['zones.geojson', 'stations.json', 'graph.json', 'fares.json']
      .map((f) => fetch(DATA + f).then((r) => {
        if (!r.ok) throw new Error(`${f}: ${r.status}`);
        return r.json();
      })),
  );

  state.zones = zones;
  state.stations = stations;
  state.fares = fares;
  state.graph = {
    adjacency: graphRaw.adjacency,
    maskLo: graphRaw.edge_mask_lo,
    maskHi: graphRaw.edge_mask_hi,
    zoneBits: graphRaw.zone_bits,
    zoneWeights: graphRaw.zone_weights,
    zoneNumbers: graphRaw.zone_numbers,
  };

  document.getElementById('valid-from').textContent = fares.valid_from;

  map = new maplibregl.Map({
    container: 'map',
    style: {
      version: 8,
      sources: {
        swisstopo: {
          type: 'raster',
          tiles: ['https://wmts.geo.admin.ch/1.0.0/ch.swisstopo.pixelkarte-grau/default/current/3857/{z}/{x}/{y}.jpeg'],
          tileSize: 256,
          maxzoom: 18,
          attribution: '&copy; <a href="https://www.swisstopo.admin.ch/">swisstopo</a>',
        },
      },
      layers: [{ id: 'basemap', type: 'raster', source: 'swisstopo' }],
    },
    center: MAP_CENTRE,
    zoom: 9.4,
    attributionControl: false,
  });
  map.addControl(new maplibregl.NavigationControl({ showCompass: false }), 'top-right');
  map.addControl(new maplibregl.AttributionControl({ compact: true }), 'bottom-right');

  await new Promise((res) => map.on('load', res));

  map.addSource('zones', { type: 'geojson', data: zones });
  map.addLayer({
    id: 'zone-fill',
    type: 'fill',
    source: 'zones',
    paint: { 'fill-color': '#cccccc', 'fill-opacity': 0.05 },
  });
  map.addLayer({
    id: 'zone-line',
    type: 'line',
    source: 'zones',
    paint: { 'line-color': '#54606b', 'line-width': 0.7, 'line-opacity': 0.55 },
  });

  // The route to whatever stop is under the cursor.
  map.addSource('route', { type: 'geojson', data: { type: 'FeatureCollection', features: [] } });
  map.addLayer({
    id: 'route-casing',
    type: 'line',
    source: 'route',
    layout: { 'line-cap': 'round', 'line-join': 'round' },
    paint: { 'line-color': '#ffffff', 'line-width': 5, 'line-opacity': 0.9 },
  });
  map.addLayer({
    id: 'route-line',
    type: 'line',
    source: 'route',
    layout: { 'line-cap': 'round', 'line-join': 'round' },
    paint: { 'line-color': '#0b1f33', 'line-width': 2.2 },
  });

  map.addSource('stations', {
    type: 'geojson',
    data: {
      type: 'FeatureCollection',
      features: stations.map((s, i) => ({
        type: 'Feature',
        id: i,
        properties: { name: s.name, zone_id: s.zone_id },
        geometry: { type: 'Point', coordinates: [s.lon, s.lat] },
      })),
    },
  });
  map.addLayer({
    id: 'station-dot',
    type: 'circle',
    source: 'stations',
    paint: {
      'circle-radius': [
        'case',
        ['boolean', ['feature-state', 'hover'], false], 7,
        ['interpolate', ['linear'], ['zoom'], 9, 1.3, 14, 3.2],
      ],
      'circle-color': [
        'case',
        ['boolean', ['feature-state', 'hover'], false], '#c2382c',
        '#20303d',
      ],
      'circle-stroke-color': '#ffffff',
      'circle-stroke-width': ['case', ['boolean', ['feature-state', 'hover'], false], 2, 0],
      'circle-opacity': [
        'case',
        ['boolean', ['feature-state', 'hover'], false], 1,
        ['interpolate', ['linear'], ['zoom'], 9, 0.28, 13, 0.7],
      ],
    },
  });

  map.addSource('origin', { type: 'geojson', data: { type: 'FeatureCollection', features: [] } });
  map.addLayer({
    id: 'origin-ring',
    type: 'circle',
    source: 'origin',
    paint: {
      'circle-radius': 8,
      'circle-color': 'rgba(0,0,0,0)',
      'circle-stroke-color': '#0b1f33',
      'circle-stroke-width': 3,
    },
  });

  const popup = new maplibregl.Popup({
    closeButton: false, closeOnClick: false, offset: 10, maxWidth: '320px',
  });

  // Hovering snaps to the nearest stop, the same rule clicking uses, so small
  // dots do not have to be hit exactly.
  // Coalesced to one update per frame: mousemove fires far faster than the
  // route line and popup need redrawing.
  let queued = null;
  let scheduled = false;
  map.on('mousemove', (e) => {
    queued = e.lngLat;
    if (scheduled) return;
    scheduled = true;
    requestAnimationFrame(() => {
      scheduled = false;
      if (queued) showHover(queued, popup);
    });
  });
  map.on('mouseout', () => { popup.remove(); clearHover(); });

  map.on('click', (e) => setOrigin(nearestStation(e.lngLat.lng, e.lngLat.lat)));
  map.getCanvas().style.cursor = 'crosshair';

  const slider = document.getElementById('budget');
  slider.addEventListener('input', () => {
    state.budget = parseFloat(slider.value);
    state.budgetWanted = state.budget;
    document.getElementById('budget-value').textContent = `CHF ${state.budget.toFixed(2)}`;
    if (state.origin !== null) paintZones();
  });

  for (const el of document.querySelectorAll('input[name=klass]')) {
    el.addEventListener('change', (ev) => { state.klass = ev.target.value; repriceOnly(); });
  }
  for (const el of document.querySelectorAll('input[name=reduction]')) {
    el.addEventListener('change', (ev) => {
      state.reduced = ev.target.value === 'reduced';
      repriceOnly();
    });
  }

  // Open on Zürich HB so the map is never empty on first paint.
  const hb = stations.findIndex((s) => s.name === 'Zürich HB');
  setOrigin(hb >= 0 ? hb : 0);
}

main().catch((err) => {
  document.getElementById('legend-items').innerHTML =
    `<div class="hint">Could not load data: ${err.message}</div>`;
  console.error(err);
});
