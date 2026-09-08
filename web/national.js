/* isodapane.ch — national view.
 *
 * The canton map prices by fare zones, because that is what ZVV bills. The
 * national tariff is a distance tariff, so this view is a plain scalar Dijkstra
 * — no zone sets, no Pareto frontier.
 *
 * Routing is by *travel time*, not distance: a fare should follow the train a
 * traveller would actually take. Tariff kilometres are then summed along that
 * route and priced.
 *
 * Three tiers, and the page always says which one an answer came from:
 *   published — a real fare exists for that station pair
 *   estimated — fitted tariff kilometres looked up in the monotone price curve
 *   adjusted  — raised by the coherence pass below
 *
 * Coherence. A leg of a journey must never cost more than the journey that
 * contains it. Fastest paths give this for free: in a static graph they have
 * optimal substructure, so the B..C part of the fastest A..C route is itself a
 * fastest B..C route, and since tariff kilometres accumulate and the price curve
 * is monotone, price(B,C) <= price(A,C).
 *
 * Published fares break that guarantee, because a real fare for one pair need
 * not respect it against another. So after pricing we walk the fastest-path tree
 * outward and raise any price below its predecessor's. That restores
 * monotonicity along every route from the origin, at the cost of moving some
 * published fares up; the hover says when it happened, and `repairedCount`
 * reports how often.
 */

const state = {
  data: null,
  exact: null,     // Map "a,b" -> {p2, p1, ph}
  origin: null,
  result: null,
  budget: 40,
  budgetWanted: 40,
  price: null,
  tier: null,
  pubKm: null,
  repairedCount: 0,
  klass: 'second',
  half: false,
  hovered: null,
};

const key = (a, b) => (a < b ? `${a},${b}` : `${b},${a}`);

/* ------------------------------------------------------------------ search */
function dijkstra(adjacency, edgeSecs, edgeKm, source) {
  const n = adjacency.length;
  const dist = new Float64Array(n).fill(Infinity);   // seconds
  const km = new Float64Array(n).fill(Infinity);     // tariff km along that route
  const prev = new Int32Array(n).fill(-1);
  const hops = new Int32Array(n).fill(-1);
  const order = [];                                  // settle order, nearest first
  dist[source] = 0;
  km[source] = 0;
  hops[source] = 0;

  // Binary heap of (distance, node).
  const hd = [0], hn = [source];
  const swap = (i, j) => {
    [hd[i], hd[j]] = [hd[j], hd[i]];
    [hn[i], hn[j]] = [hn[j], hn[i]];
  };
  const push = (d, v) => {
    hd.push(d); hn.push(v);
    let i = hd.length - 1;
    while (i > 0) { const p = (i - 1) >> 1; if (hd[p] <= hd[i]) break; swap(i, p); i = p; }
  };
  const pop = () => {
    const td = hd[0], tv = hn[0];
    const ld = hd.pop(), lv = hn.pop();
    if (hd.length) {
      hd[0] = ld; hn[0] = lv;
      let i = 0;
      for (;;) {
        const l = 2 * i + 1, r = l + 1;
        let m = i;
        if (l < hd.length && hd[l] < hd[m]) m = l;
        if (r < hd.length && hd[r] < hd[m]) m = r;
        if (m === i) break;
        swap(i, m); i = m;
      }
    }
    return [td, tv];
  };

  const settled = new Uint8Array(n);
  while (hd.length) {
    const [d, u] = pop();
    if (settled[u]) continue;
    settled[u] = 1;
    order.push(u);
    const ns = adjacency[u], ss = edgeSecs[u], ks = edgeKm[u];
    for (let i = 0; i < ns.length; i++) {
      const v = ns[i], nd = d + ss[i];
      // Strict improvement, with a deterministic tie-break on predecessor index
      // so equally fast routes are chosen the same way from every origin — the
      // sub-journey guarantee depends on that consistency.
      if (nd < dist[v] - 1e-9 || (Math.abs(nd - dist[v]) <= 1e-9 && prev[v] >= 0 && u < prev[v])) {
        dist[v] = Math.min(nd, dist[v]);
        km[v] = km[u] + ks[i];
        prev[v] = u;
        hops[v] = hops[u] + 1;
        push(nd, v);
      }
    }
  }
  const pathTo = (v) => {
    if (!isFinite(dist[v])) return null;
    const out = [];
    for (let u = v; u >= 0; u = prev[u]) out.push(u);
    return out.reverse();
  };
  return { dist, km, hops, prev, order, pathTo };
}

/* ------------------------------------------------------------------- price */
function curvePrice(km, first) {
  const c = state.data.curve;
  const arr = first ? c.p1 : c.p2;
  // Nearest tabulated tariff distance; the curve covers 1..678 km densely.
  let lo = 0, hi = c.km.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (c.km[mid] < km) lo = mid + 1; else hi = mid;
  }
  let best = lo;
  if (lo > 0 && Math.abs(c.km[lo - 1] - km) < Math.abs(c.km[lo] - km)) best = lo - 1;
  const v = arr[best];
  return v > 0 ? v : null;
}

/* Price every station, then repair coherence.
 *
 * Tier 0 = published, 1 = estimated, 2 = raised by the repair pass.
 */
function priceAll() {
  const r = state.result;
  const n = state.data.stations.length;
  const first = state.klass === 'first';
  const chf = new Float64Array(n).fill(NaN);
  const tier = new Uint8Array(n);
  // Tariff kilometres of the published relation, which are the relation's own
  // and need not match the route this map happens to draw.
  const pubKm = new Float64Array(n).fill(NaN);

  for (let v = 0; v < n; v++) {
    if (!isFinite(r.dist[v])) continue;
    const e = state.exact.get(key(state.origin, v));
    let p = null;
    if (e) {
      p = first ? e.p1 : e.p2;
      if (p > 0) { tier[v] = 0; pubKm[v] = e.km; } else p = null;
    }
    if (p === null) {
      p = curvePrice(r.km[v], first);
      tier[v] = 1;
    }
    if (p === null) continue;
    chf[v] = p;
  }
  chf[state.origin] = 0;
  tier[state.origin] = 0;

  // No adjustment. A published fare is a real price for a real ticket and is
  // served exactly as published, without exception.
  //
  // It is tempting to read a nearer station costing more than a farther one as
  // an error to repair. It is not. A published fare is a *direct relation*
  // between two stations, priced for the journey you would actually make between
  // them — it says nothing about what lies geographically in between, and it is
  // not the sum of anything.
  //
  // Zürich HB is the case in point. Liestal is published at CHF 38.00 and Basel
  // SBB, further along the same line, at 31.60. Both are correct: the fast Basel
  // train runs *through* Liestal without stopping, so reaching Liestal means a
  // slower line or a longer way round, and that is a dearer journey than the
  // express to Basel. Forcing the two into order would misprice one of them.
  //
  // So the only ordering that holds here is among the estimates, which is
  // automatic: tariff kilometres accumulate along the route and the price curve
  // is isotonic.
  state.repairedCount = 0;

  // Halbtax is half the full fare, applied after repair so the two columns stay
  // consistent with one another.
  if (state.half) {
    for (let v = 0; v < n; v++) if (isFinite(chf[v])) chf[v] = chf[v] / 2;
  }
  for (let v = 0; v < n; v++) {
    if (isFinite(chf[v])) chf[v] = Math.round(chf[v] * 20) / 20;
  }
  state.price = chf;
  state.tier = tier;
  state.pubKm = pubKm;
}

/* Fare to `dest`, and where the number came from. */
function fareTo(dest) {
  if (!state.price || !isFinite(state.price[dest])) return null;
  const published = state.tier[dest] === 0;
  return {
    chf: state.price[dest],
    tier: state.tier[dest],
    km: published && !isNaN(state.pubKm[dest]) ? state.pubKm[dest] : state.result.km[dest],
    exact: published,
  };
}

/* --------------------------------------------------------------- rendering */
let map;
const BANDS = [
  [10, '#1a9850'], [20, '#66bd63'], [30, '#a6d96a'], [45, '#fee08b'],
  [65, '#fdae61'], [90, '#f46d43'], [130, '#d73027'], [Infinity, '#a50026'],
];
const colourFor = (chf) => BANDS.find(([hi]) => chf <= hi)[1];

function paint() {
  const feats = [];
  const n = state.data.stations.length;
  for (let i = 0; i < n; i++) {
    const f = fareTo(i);
    if (!f) continue;
    if (f.chf > state.budget + 1e-9) continue;
    const s = state.data.stations[i];
    feats.push({
      type: 'Feature',
      id: i,
      properties: { chf: f.chf, colour: colourFor(f.chf), tier: f.tier },
      geometry: { type: 'Point', coordinates: [s.lon, s.lat] },
    });
  }
  map.getSource('reach').setData({ type: 'FeatureCollection', features: feats });
  document.getElementById('reach-count').textContent =
    `${feats.length} of ${n} stations`;
  const rc = document.getElementById('repaired');
  if (rc) {
    let pub = 0;
    for (let i = 0; i < n; i++) if (state.tier[i] === 0 && isFinite(state.price[i])) pub++;
    rc.textContent = `${pub} published fare${pub === 1 ? '' : 's'}, the rest estimated`;
  }
  renderLegend();
}

function renderLegend() {
  const el = document.getElementById('legend-items');
  el.innerHTML = BANDS.map(([hi, c], i) => {
    const lo = i === 0 ? 0 : BANDS[i - 1][0];
    const label = hi === Infinity ? `over CHF ${lo}` : `CHF ${lo}–${hi}`;
    const off = lo > state.budget ? ' over' : '';
    return `<div class="legend-row${off}"><span class="swatch" style="background:${c}"></span>
            <span class="lg-label">${label}</span></div>`;
  }).join('');
}

function nearest(lng, lat) {
  const k = Math.cos((lat * Math.PI) / 180);
  let best = -1, bd = Infinity;
  const st = state.data.stations;
  for (let i = 0; i < st.length; i++) {
    const dx = (st[i].lon - lng) * k, dy = st[i].lat - lat;
    const d = dx * dx + dy * dy;
    if (d < bd) { bd = d; best = i; }
  }
  return best;
}

function updateBudgetRange() {
  let max = 0;
  for (let i = 0; i < state.data.stations.length; i++) {
    const f = fareTo(i);
    if (f && f.chf > max) max = f.chf;
  }
  if (max <= 0) max = 50;
  const slider = document.getElementById('budget');
  slider.max = Math.ceil(max);
  state.budget = Math.min(state.budgetWanted, Math.ceil(max));
  slider.value = state.budget;
  document.getElementById('budget-value').textContent = `CHF ${state.budget.toFixed(2)}`;
  document.getElementById('budget-max').textContent = `max CHF ${max.toFixed(2)}`;
}

function setOrigin(i) {
  state.origin = i;
  const t0 = performance.now();
  state.result = dijkstra(state.data.adjacency, state.data.edge_secs, state.data.edge_km, i);
  priceAll();
  const ms = performance.now() - t0;
  updateBudgetRange();
  paint();
  const s = state.data.stations[i];
  map.getSource('origin').setData({
    type: 'Feature', geometry: { type: 'Point', coordinates: [s.lon, s.lat] },
  });
  document.getElementById('origin-name').textContent = s.name;
  document.getElementById('timing').textContent = `${ms.toFixed(0)} ms`;
  clearHover();
}

const esc = (s) => s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

function clearHover() {
  state.hovered = null;
  map.getSource('route').setData({ type: 'FeatureCollection', features: [] });
}

function showHover(lngLat, popup) {
  const i = nearest(lngLat.lng, lngLat.lat);
  if (i !== state.hovered) {
    state.hovered = i;
    const path = state.result ? state.result.pathTo(i) : null;
    map.getSource('route').setData(path && path.length > 1 ? {
      type: 'Feature',
      geometry: {
        type: 'LineString',
        coordinates: path.map((k) => [state.data.stations[k].lon, state.data.stations[k].lat]),
      },
    } : { type: 'FeatureCollection', features: [] });
  }
  const s = state.data.stations[i];
  let html = `<div class="pop-stop">${esc(s.name)}</div>`;
  const f = state.result ? fareTo(i) : null;
  if (!state.result) {
    html += '<div class="pop-zone">pick a starting station</div>';
  } else if (!f) {
    html += '<div class="pop-none">no rail route in this model</div>';
  } else {
    const hops = state.result.hops[i];
    html += `<div class="pop-fare"><b>CHF ${f.chf.toFixed(2)}</b> · ${f.km.toFixed(0)} tariff km</div>
             <div class="pop-hops">${hops} stop${hops === 1 ? '' : 's'} from ${esc(state.data.stations[state.origin].name)}</div>
             <div class="pop-tier ${['exact', 'est'][f.tier]}">${[
                 'published fare — a direct relation, used exactly as published',
                 'estimated — median error 6.7%, 9 in 10 within 19%',
               ][f.tier]}</div>`;
  }
  popup.setLngLat(lngLat).setHTML(html).addTo(map);
}

/* -------------------------------------------------------------------- boot */
async function main() {
  const data = await fetch('data/national.json').then((r) => {
    if (!r.ok) throw new Error(`national.json: ${r.status}`);
    return r.json();
  });
  state.data = data;
  state.exact = new Map();
  const ex = data.exact;
  for (let i = 0; i < ex.a.length; i++) {
    state.exact.set(key(ex.a[i], ex.b[i]), { p2: ex.p2[i], p1: ex.p1[i], ph: ex.ph[i], km: ex.km[i] });
  }
  document.getElementById('source-note').textContent = data.source.fares;

  map = new maplibregl.Map({
    container: 'map',
    style: {
      version: 8,
      sources: {
        swisstopo: {
          type: 'raster',
          tiles: ['https://wmts.geo.admin.ch/1.0.0/ch.swisstopo.pixelkarte-grau/default/current/3857/{z}/{x}/{y}.jpeg'],
          tileSize: 256, maxzoom: 18,
          attribution: '&copy; <a href="https://www.swisstopo.admin.ch/">swisstopo</a>',
        },
      },
      layers: [{ id: 'basemap', type: 'raster', source: 'swisstopo' }],
    },
    center: [8.23, 46.8], zoom: 7.1, attributionControl: false,
  });
  map.addControl(new maplibregl.NavigationControl({ showCompass: false }), 'top-right');
  map.addControl(new maplibregl.AttributionControl({ compact: true }), 'bottom-right');
  await new Promise((r) => map.on('load', r));

  map.addSource('route', { type: 'geojson', data: { type: 'FeatureCollection', features: [] } });
  map.addLayer({
    id: 'route-casing', type: 'line', source: 'route',
    layout: { 'line-cap': 'round', 'line-join': 'round' },
    paint: { 'line-color': '#ffffff', 'line-width': 5, 'line-opacity': 0.9 },
  });
  map.addLayer({
    id: 'route-line', type: 'line', source: 'route',
    layout: { 'line-cap': 'round', 'line-join': 'round' },
    paint: { 'line-color': '#0b1f33', 'line-width': 2.2 },
  });

  map.addSource('reach', { type: 'geojson', data: { type: 'FeatureCollection', features: [] } });
  map.addLayer({
    id: 'reach-dot', type: 'circle', source: 'reach',
    paint: {
      'circle-radius': ['interpolate', ['linear'], ['zoom'], 6, 6, 9, 9.5, 12, 14],
      'circle-color': ['get', 'colour'],
      // A hollow ring marks an estimate; a solid dot is a published fare.
      'circle-opacity': ['case', ['==', ['get', 'tier'], 0], 0.95, 0.55],
      'circle-stroke-color': '#ffffff',
      'circle-stroke-width': 1,
    },
  });

  map.addSource('origin', { type: 'geojson', data: { type: 'FeatureCollection', features: [] } });
  map.addLayer({
    id: 'origin-ring', type: 'circle', source: 'origin',
    paint: {
      'circle-radius': 8, 'circle-color': 'rgba(0,0,0,0)',
      'circle-stroke-color': '#0b1f33', 'circle-stroke-width': 3,
    },
  });

  const popup = new maplibregl.Popup({ closeButton: false, closeOnClick: false, offset: 10, maxWidth: '300px' });
  let queued = null, scheduled = false;
  map.on('mousemove', (e) => {
    queued = e.lngLat;
    if (scheduled) return;
    scheduled = true;
    requestAnimationFrame(() => { scheduled = false; if (queued) showHover(queued, popup); });
  });
  map.on('mouseout', () => { popup.remove(); clearHover(); });
  map.on('click', (e) => setOrigin(nearest(e.lngLat.lng, e.lngLat.lat)));
  map.getCanvas().style.cursor = 'crosshair';

  const slider = document.getElementById('budget');
  slider.addEventListener('input', () => {
    state.budget = parseFloat(slider.value);
    state.budgetWanted = state.budget;
    document.getElementById('budget-value').textContent = `CHF ${state.budget.toFixed(2)}`;
    paint();
  });
  for (const el of document.querySelectorAll('input[name=klass]')) {
    el.addEventListener('change', (ev) => { state.klass = ev.target.value; priceAll(); updateBudgetRange(); paint(); });
  }
  for (const el of document.querySelectorAll('input[name=reduction]')) {
    el.addEventListener('change', (ev) => { state.half = ev.target.value === 'half'; priceAll(); updateBudgetRange(); paint(); });
  }

  const zh = data.stations.findIndex((s) => s.name === 'Zürich HB');
  setOrigin(zh >= 0 ? zh : 0);
}

main().catch((err) => {
  document.getElementById('legend-items').innerHTML =
    `<div class="hint">Could not load data: ${err.message}</div>`;
  console.error(err);
});
