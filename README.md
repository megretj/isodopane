# isodapane.ch — Canton of Zürich

An **isodapane** map: the monetary-budget analogue of an isochrone map. Pick a
starting point on a map of the Canton of Zürich, set a budget in Swiss francs,
and see everywhere a single ZVV ticket takes you for that money.

Same feel as an isochrone map — click a point, drag a slider, watch the reachable
area grow — but the contours are francs, not minutes.

Fully static: a Rust build pipeline emits four JSON files, and a single HTML page
with MapLibre runs the fare search in the browser on every click. No backend, no
database, no API keys.

```
cargo run -p isodapane-build -- fetch-zones
cargo run -p isodapane-build -- fetch-gtfs
cargo run -p isodapane-build -- build-zones
cargo run -p isodapane-build -- build-graph
cargo run -p isodapane-build -- build-fares
cargo run -p isodapane-build -- validate --sample 400

cd web && python3 -m http.server 8000   # then open localhost:8000
```

---

## Discovered data facts

Everything downstream keys off these, so they are recorded rather than guessed.

| Thing | Value |
|---|---|
| WFS endpoint | `https://maps.zh.ch/wfs/OGDZHWFS` |
| Tarifzonen typename | `ms:ogd-0348_giszhpub_zvv_tarifzonen_f` |
| Zone number attribute | `zone` (integer) |
| Features returned | 60, of which **45 carry a zone number** |
| GTFS feed | `data.opentransportdata.swiss/.../timetable-2026-gtfs2020/permalink` |
| GTFS build used | `gtfs_fp2026_20260905.zip`, 237 MB zipped / 3.8 GB unpacked |
| Fare table | ZVV *Tickets und Preise*, valid from **2025-12-14** |

### The WFS does not reproject

The service advertises `EPSG:4326`, and if you ask for it, it *labels* the
response `EPSG:4326` — but the coordinates come back in LV95 (EPSG:2056)
regardless. This is true for the short `srsName=EPSG:4326` form, the URN form,
GeoJSON output and GML output alike. Only WFS 1.0.0 is honest enough to admit it
is sending 2056.

So reprojection is unavoidable. Rather than take on `proj` as a native
dependency, `crs.rs` implements swisstopo's published approximate LV95→WGS84
formula in about forty lines. Measured against swisstopo's own reference point
(Zimmerwald observatory) it lands **0.24 m** off — two orders of magnitude finer
than the ~15 m tolerance we then simplify the polygons with, so it is exact for
this purpose. There is a unit test pinning that.

### 14 zone features carry no zone number

They arrive with `farbe_plan = 1111` and cover the lakes and unserved forest
ridges. They hold no tariff information, so they are dropped with a warning; a
stop falling only inside one is treated as out of network, like a stop outside
the canton. Zone 133 arrives as two separate polygons and is dissolved into one.

---

## The fare model, and where the original design was wrong

ZVV prices a journey by the **number of distinct fare zones** it covers, each
counted once, with zones 110 (Zürich) and 120 (Winterthur) each counting as two.
The search therefore minimises the weight of a *set*, not a scalar, which is why
it carries zone sets as `u64` bitmasks and keeps a Pareto frontier per station
(see `search.rs` for why one label per node loses the optimum).

**Two things turned up during the build that the original spec got wrong.**

### 1. A ticket must cover zones *travelled through*, not just stopped in

The spec's relaxation rule was `set(u) | bit(zone(v))` — add the arrival stop's
zone. That undercharges every non-stop service. ZVV tells passengers to
"determine the number of zones you will pass through", and the zone plan is read
along the route, so a ticket must cover zones the vehicle crosses without
stopping.

Concretely: the non-stop IC from Zürich HB to Winterthur appears in `stop_times`
as a single consecutive pair. Under the spec's rule that bills 110 + 120 = 4
zones, CHF 9.40. The real ticket also needs zones 121 and 122, which the train
runs straight through: 6 zones, **CHF 13.60**. Same story for Zürich HB → Uster
(zone 130 at Nänikon-Greifensee) and Wetzikon → Rapperswil (zone 133 at Bubikon).

So each **edge** now carries the set of zones its vehicle traverses, and
relaxation unions that whole set. Everything else — dominance, the Pareto
frontier, the heap ordering — is unchanged.

### 2. Getting the traversed set without route geometry

This GTFS feed has **no `shapes.txt`**, so the true alignment is unavailable. Two
mechanisms approximate it:

- **Straight-line sampling.** Each edge's zone set is sampled every ~250 m along
  the chord between its two stops. Exact for short hops, where both stops sit in
  the same or adjacent zones.
- **Shortcut removal.** The chord is unreliable for long express hops, which is
  exactly where it matters. But an express runs on the same tracks as the
  stopping service beside it, so where a path of *shorter* edges already joins
  the same pair without a big detour (≤ 1.6× the direct distance, ≤ 12 hops),
  the express edge adds nothing but its bad zone estimate and is dropped. The
  search then follows the local alignment and picks up the right zones.

  This removed **611 of 3831 edges** and stranded nothing new. A blanket
  distance threshold was tried first and rejected: dropping all edges over 4 km
  strands 112 stations, because rural bus routes legitimately have widely spaced
  stops. Those survive here and keep their chord estimate.

This is the weakest part of the model and is stated as a limitation in the UI.
`shapes.txt` from another feed is the exact fix if it is ever needed.

---

## Measured results

### Fare fixtures

`validate` checks six origin/destination pairs. Expected values are derived from
the official Tarifzonenplan plus ZVV's traversal rule and priced from
`data/fares.toml` — derived independently of the search, not read back out of it.
Each fixture records its zone chain so the derivation is auditable.

| From | To | Zones | 2nd class | Zone chain |
|---|---|---|---|---|
| Zürich HB | Zürich Oerlikon | 2 | 4.70 | {110×2} |
| Zürich HB | Winterthur | 6 | 13.60 | {110×2, 121, 122, 120×2} |
| Zürich HB | Uster | 5 | 11.40 | {110×2, 121, 130, 131} |
| Zürich HB | Dietikon | 3 | 7.20 | {110×2, 154} |
| Winterthur | Uster | 6 | 13.60 | {120×2, 122, 121, 130, 131} |
| Wetzikon ZH | Rapperswil SG | 4 | 9.40 | {132, 133, 134, 180} |

All six pass in Rust and in the browser port. Zürich HB → Oerlikon prices as
Tarifstufe 2, not 1, confirming zone 110 bills as two zones.

> **Still to confirm before shipping:** these six prices are derived from the
> published zone plan and tariff rules, not read off a ZVV sales channel. Spot-
> check them in the ZVV app. A mismatch is a bug in the graph or the zone
> assignment — never adjust the price table to make a fixture pass.

### Label growth — the cap does bind, and 16 was not safe

The spec expected one to three labels per station. **That is not what this
network does.** Measured over 460 origins and 1,269,600 station pairs:

| `LABEL_CAP` | max labels | mean labels | fares differing from uncapped |
|---|---|---|---|
| 16 | 16 | 10.73 | **15 of 1,269,600 (0.0012%)**, worst overcharge 1 zone |
| 32 | 32 | 18.61 | 0 |
| **64 (shipped)** | 64 | 25.97 | **0** |
| uncapped | 131 | 27.17 | — |

So the originally suggested cap of 16 was genuinely lossy. **We ship 64**: exact
on every measured pair, with 2× margin over the smallest exact cap (32) and
comfortably under the 131 the frontier actually reaches. `validate` reports the
"priced differently" count on every run; a non-zero value means fares have become
upper bounds and needs investigating.

### Graph connectivity

2760 stations and 3220 edges after shortcut removal, in **10 components**:

| Size | What it is |
|---|---|
| 2712 | the network proper |
| 27 | Zürichsee boat piers (`… (See)`) |
| 4 | Greifensee boat piers |
| 3 | Feuerthalen / Langwiesen, over the Rhine at the canton edge |
| 3 | Obersee boat piers (Lachen, Schmerikon, Rapperswil Hochschule) |
| 3 | Ober-/Niederneunforn, over the Thurgau border |
| 2 ×4 | Polybahn; Felsenegg cable car; Meilen–Horgen car ferry; Langwiesen |

These are real characteristics of the timetable data, not a build bug: boat,
funicular and ferry stops are separate GTFS stops from the land stops beside
them, with no service linking the two. 48 of 2760 stations (1.7%) sit outside the
main component and read as unreachable.

Consequence: **zones 182 and 183 can never be priced**, because their only stops
are Obersee boat piers. Every other zone is reachable. Because a zone's displayed
fare is the minimum over the stops inside it, and the land stops dominate, the
map itself is unaffected elsewhere.

### Routes shown on hover

The browser search additionally keeps, per label, a pointer to the label it came
from — one integer per label, and it changes no fare. Hovering anywhere snaps to
the nearest stop (the same rule clicking uses, so small dots need not be hit
exactly) and shows its fare, how many stops away it is, and the winning route,
which is also drawn on the map.

Those routes are checked, not assumed: over **1,225,969 reconstructed routes**,
every one is a real walk in the graph, starts at the origin, ends at the stop
asked for, has a hop count matching the label, and has a zone union that
reproduces the reported fare exactly. The longest route in the canton is 141
stops.

The budget slider's ceiling is the priciest reachable place for the selected
price column — CHF 18.00 for adult 2nd class, 29.80 for 1st, 9.00 reduced 2nd —
so the top of the slider always means "everywhere you can get to". A budget
above a lower ceiling is remembered rather than discarded, so toggling to the
reduced column and back does not quietly shrink it.

### Builds are reproducible

`gtfs-structures` hands back stops in a `HashMap`, so iterating it directly
numbered the stations differently on every run: `stations.json` and `graph.json`
came out different byte-for-byte from identical inputs, and any cached
cross-check silently compared mismatched indices. Stop ids are now walked in
sorted order, and two consecutive builds produce byte-identical output.

### Performance

A search from one origin covers every destination: **~12 ms in Rust**, **32–50 ms
in the browser**. Payload is 490 KB total (zones 97 KB, stations 289 KB, graph
101 KB, fares 2.5 KB). The budget slider only re-filters and never re-searches;
class and reduction toggles only reselect a price column.

---

## Layout

```
crates/build/src/
  main.rs        clap subcommands
  crs.rs         LV95 -> WGS84 (see "the WFS does not reproject")
  wfs.rs         zone + GTFS fetching
  zones.rs       reproject, dissolve, simplify
  stations.rs    GTFS stops, parent collapsing, PiP, traversed zones
  graph.rs       edge set, shortcut removal, connectivity
  search.rs      multi-label Pareto search
  zoneset.rs     u64 bitmask, weights, the 110/120 double rule
  fares.rs       price table
  validate.rs    fixtures + label growth
data/fares.toml  hand-maintained prices, re-verify each December
web/             index.html, app.js (JS port of search.rs), style.css, data/
```

The JS search in `app.js` is a direct port of `search.rs`. They are checked
against each other with `validate --dump`, which writes per-origin results the JS
is diffed against: **0 of 1,269,600 station pairs differ**. The browser copy adds
route reconstruction, which the build side does not need; the fare logic is
identical. Keep them in step; if the duplication becomes annoying, compile the
Rust to WASM and delete the JS.

Zone sets are split into two 32-bit halves in JavaScript. A 45-bit mask survives
a JSON round trip as a float, but `|` coerces to int32 and would silently
truncate it.

## Attribution and licence

- Fare zones: **Kanton Zürich (OGD)**, GIS-ZH Nr. 348, opendata.swiss
  "Reference Required" (BY) — attribution mandatory, shown in the UI footer.
- Basemap: **© swisstopo**, `wmts.geo.admin.ch`.
- Timetable: **opentransportdata.swiss** GTFS.

Code is MIT, see `LICENSE`.

## Phase 2 — national coverage

`web/national.html` prices rail travel across Switzerland. The national tariff
charges by **tariff kilometres**, not zones, so it is a plain scalar Dijkstra
rather than the canton map's Pareto search.

```
cargo run -p isodapane-build -- fetch-national
cargo run -p isodapane-build -- build-national         # parse OSDM -> data/national/
cargo run -p isodapane-build -- build-national-graph   # rail graph from GTFS
cargo run -p isodapane-build -- calibrate-national     # fit tariff km, report error
cargo run -p isodapane-build -- build-national-web     # -> web/data/national.json
cargo run -p isodapane-build -- fetch-fare-unions
cargo run -p isodapane-build -- build-fare-unions
```

### The T601 plan did not survive, and the replacement is better

The spec planned to transcribe the **T601** price table from a PDF and warned
that tariff kilometres "are not published machine-readably".

**T601, T603 and T604 are login-gated.** Every URL form on
`allianceswisspass.ch` returns the Alliance SwissPass login page rather than the
document, so transcribing T601 from open sources is not possible.

It is also unnecessary. opentransportdata.swiss publishes an
[OSDM offline file](https://data.opentransportdata.swiss/en/dataset/osdm-offline)
carrying, per relation, the **tariff distance**, the **permitted via-route** and
the **actual price** by class and reduction card — the price table and the
tariff kilometres together. See `data/national/PROVENANCE.md`.

### Routing follows the fastest train

Fares are computed on the line a traveller would actually take, so routing is a
Dijkstra over **scheduled run times** (the fastest observed run on each stretch),
not over distance. Tariff kilometres are then summed along that route.

Two data problems had to be fixed before this behaved:

- **Switzerland only.** The GTFS feed carries foreign stations, and the router
  cheerfully sent Bellinzona to Intragna (38 km) the long way round through
  Domodossola — 377 tariff km. Stops are now filtered to Swiss DIDOK numbers
  (85xxxxx), dropping 20,543 foreign stops.
- **Co-located stations.** "Locarno" and "Locarno FART" are separate nodes
  sharing no service, so the Centovalli line hung off the Swiss network entirely
  and was reachable only through Italy. Stations within 400 m are now joined by a
  walking link that costs time but no tariff kilometres — 38 such links.

### Two tiers, and the map says which

- **Published** — a real fare exists for that station pair (2084 pairs), served
  exactly as published. Zürich HB → Bern is the real CHF 51.00, Basel SBB 31.60,
  Genève 81.80, Lugano 65.00, Chur 41.00, Zermatt 125.00. A published pair also
  reports its own tariff kilometres, which are the relation's and need not match
  the route this map happens to draw.
- **Estimated** — no published fare, so fitted tariff kilometres along the
  fastest route are looked up in the price curve. Median error 6.7%, nine in ten
  within 19%.

### Published fares are authoritative, without exception

A published fare is a real price for a real ticket. It is served exactly as
published — never scaled, capped, floored or otherwise adjusted. Measured over
374,797 priced pairs from 219 origins: **732 of 732 published fares served
unchanged, 100%**.

It is tempting to read a nearer station costing more than a farther one as an
error to repair. It is not. **A published fare is a direct relation between two
stations**, priced for the journey you would actually make between them; it says
nothing about what lies geographically in between, and it is not the sum of
anything. From Zürich HB:

| station | published | served |
|---|---|---|
| Aarau | 22.80 | 22.80 |
| Olten | 27.00 | 27.00 |
| **Liestal** | **38.00** | **38.00** |
| Basel SBB | 31.60 | 31.60 |

Liestal costs more than Basel, further along the same line, and both are right:
the fast Basel train runs *through* Liestal without stopping, so reaching Liestal
means a slower line or a longer way round — a dearer journey than the express to
Basel. Forcing the two into order would misprice one of them.

Ordering therefore only applies among the estimates, where it is automatic:
tariff kilometres accumulate along the route and the price curve is isotonic. Of
371,891 consecutive estimate-to-estimate steps, **none is out of order**.

Two earlier attempts are worth recording as things not to do. The first enforced
monotonicity by *raising* prices, which moved a quarter of all published fares
upward and served Zürich → Basel at 38.00 instead of 31.60. The second capped
downward, which held Basel at 31.60 but pulled Liestal to 31.60 as well —
inventing a fare for a journey nobody makes. Both were solving a problem that was
not there.

### Regional fare unions

`fetch-fare-unions` / `build-fare-unions` pull SBB's Tarifverbundkarte into
`data/national/fare_unions.json`: **20 fare unions covering all 2137 Swiss
municipalities** — Libero, Frimobil, A-Welle, OSTWIND, ZVV, TNW, Arcobaleno,
Passepartout and the rest. That answers *which* regional tariff applies where,
which is the prerequisite for pricing local travel beyond the rail tariff.

It does not answer what those tariffs cost. None of the twenty publishes a
machine-readable zone plan or price table; the Zürich half of this project needed
a cantonal WFS plus a hand-transcribed table, and covering all twenty means
repeating that twenty times against twenty different publication practices.

A freshness check found **no newer open price data**: OSDM still offers only the
2023 delivery, the OJP Fare API is key-gated and test-only, and the tariff PDFs
remain behind a login. See `data/national/PROVENANCE.md`.

### What is honest to claim

Rail only, and only stations a train calls at — 3249 of them, since the national
tariff is a railway tariff and bus stops cannot be priced by it. The fare data is
**from 2023** and sits below current prices. Single full-fare tickets only;
Halbtax is taken as half. The portal is explicit that this data "does not
constitute a sufficient condition for ticket sales by third parties", and the
page says so too.

## Publishing

The whole thing is a static site: **upload `web/` and you are done.** No backend,
no database, no API keys, no build step.

```
web/
  index.html  national.html    the two maps
  app.js  national.js  style.css
  vendor/                      MapLibre GL JS 4.7.1, BSD-3-Clause
  data/                        generated by the pipeline, ~1.2 MB
```

MapLibre is **vendored rather than pulled from a CDN**, so the only third-party
request a visitor makes is for swisstopo basemap tiles. Every internal path is
relative, so it works at a domain root or in a subdirectory alike — verified by
serving it at `/isodapane/` and loading both pages with zero console or network
errors.

- **GitHub Pages**: push `web/` as the published directory (or set Pages to serve
  `/docs` and rename it). Project sites live under `/<repo>/`, which the relative
  paths already handle.
- **Netlify / Cloudflare Pages**: publish directory `web`, no build command.
- **Any web server**: copy `web/` into the document root.

Serve it over HTTPS: the swisstopo tiles are HTTPS, and a mixed-content page
would lose its basemap. Nothing needs configuring for the JSON payloads beyond
the usual static-file handling, though gzip is worth enabling — `data/` is mostly
JSON and compresses to roughly a third.

## Not built

No accounts, backend, database, real-time data, journey planning, ticket links,
multi-zone boundary stops, season tickets or day passes, or WASM. The national
view is rail-only and does not model regional fare unions, Z-Pass, or the
supplements some operators charge.
