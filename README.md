# isodapane

**Disclaimer** This project (including these notes) was nearly entirely vibe-coded using Anthropic Opus 5.

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-b7410e.svg)](https://www.rust-lang.org/)
[![Static site](https://img.shields.io/badge/hosting-static%20%C2%B7%20no%20backend-brightgreen.svg)](#deploying)

**How far can one ticket take you?** An *isodapane* map is the monetary analogue
of an isochrone map: pick a starting point, set a budget in Swiss francs, and see
everywhere a single ticket takes you for that money. Same feel as an isochrone
map — click a point, drag a slider, watch the reachable area grow — but the
contours are francs, not minutes.

Two maps ship in this repo:

| Map | Coverage | Fare model |
|---|---|---|
| `web/index.html` | Canton of Zürich, all ZVV modes | ZVV fare zones — a ticket must cover every zone the journey passes through |
| `web/national.html` | Swiss rail, 3249 stations | National tariff — priced by tariff kilometres, with real published fares where they exist |

Everything is static. A Rust pipeline turns open data into a handful of JSON
files, and the browser runs the fare search itself on every click — no backend,
no database, no API keys. A search from one origin prices every destination in
the network in 32–50 ms.

<!-- Add a screenshot here once you have one:
![The Zürich map](docs/screenshot.png)
-->

## Try it locally

The generated data is committed, so nothing needs building to see the maps:

```bash
git clone https://github.com/<you>/isodapane.git
cd isodapane/web
python3 -m http.server 8000
# open http://localhost:8000        (Zürich)
#      http://localhost:8000/national.html   (Switzerland)
```

Any static file server works; the pages must be *served*, not opened as
`file://`, because they fetch their data with `fetch()`.

## Rebuilding the data

Requires a recent stable Rust toolchain (edition 2021). Each subcommand is
idempotent and caches aggressively, so a rerun does not re-hit the network. The
GTFS download is ~240 MB.

```bash
# Canton of Zürich
cargo run -p isodapane-build -- fetch-zones      # Kanton Zürich WFS -> fare-zone polygons
cargo run -p isodapane-build -- fetch-gtfs       # Swiss GTFS static feed
cargo run -p isodapane-build -- build-zones      # reproject, dissolve, simplify
cargo run -p isodapane-build -- build-graph      # stations + edges + traversed zones
cargo run -p isodapane-build -- build-fares      # data/fares.toml -> web/data/fares.json
cargo run -p isodapane-build -- validate --sample 400

# Switzerland (rail)
cargo run -p isodapane-build -- fetch-national        # OSDM offline file
cargo run -p isodapane-build -- build-national        # parse OSDM -> data/national/
cargo run -p isodapane-build -- build-national-graph  # rail graph from GTFS
cargo run -p isodapane-build -- calibrate-national    # fit tariff km, report error
cargo run -p isodapane-build -- build-national-web    # -> web/data/national.json
cargo run -p isodapane-build -- fetch-fare-unions
cargo run -p isodapane-build -- build-fare-unions
```

`validate` is the regression suite: it checks fare fixtures derived independently
from the published zone plan, and reports label-growth statistics for the search.
`--dump <path>` writes per-origin results so the JavaScript port can be diffed
against the Rust one.

Builds are byte-for-byte reproducible from identical inputs.

## How it works

**Zürich — minimising a set, not a number.** ZVV prices a journey by the number
of distinct fare zones it covers, each counted once, with Zürich (110) and
Winterthur (120) each counting double. Cost is therefore a *set* union along the
path, not an additive scalar, so plain Dijkstra does not apply: the search keeps
a Pareto frontier of zone sets per station, carried as `u64` bitmasks.

Two corrections mattered more than anything else in the model:

- A ticket must cover the zones a vehicle **passes through**, not only those it
  stops in. The non-stop IC from Zürich HB to Winterthur is a single edge in
  GTFS, but the real ticket still pays for zones 121 and 122 that the train runs
  straight through — CHF 13.60, not 9.40. Each edge therefore carries the zone
  set its vehicle traverses.
- This GTFS feed has no `shapes.txt`, so traversed zones are sampled along the
  straight chord between stops, and long express "shortcut" edges are dropped
  where a path of shorter edges already joins the same pair. This is the weakest
  part of the model and the UI says so.

**Switzerland — tariff kilometres.** The national tariff is distance-based, so
this is an ordinary scalar Dijkstra — but over *scheduled run times*, because a
fare has to be computed on the line a traveller would actually take. Published
fares (2084 station pairs) are served exactly as published, never scaled, capped
or reordered; everywhere else, tariff kilometres along the fastest route are
looked up in a fitted price curve.

The full story — including the projection bug in the WFS, the label-cap
measurements, connectivity analysis, and two monotonicity "fixes" that made
things worse — is in [docs/engineering-notes.md](docs/engineering-notes.md).

## Accuracy and limitations

This is a map for planning and curiosity, **not a sales channel**. Do not use it
to decide what to pay.

Zürich:

- Six fare fixtures pass in both the Rust and JavaScript implementations, and the
  two disagree on 0 of 1,269,600 station pairs. Prices come from a
  hand-maintained table (ZVV *Tickets und Preise*, valid from 2025-12-14) and are
  derived from the published zone plan rather than read off a ZVV sales channel.
- Zones travelled through are approximated without route geometry (see above).
- 48 of 2760 stations (1.7%) — lake boats, the Polybahn, a cable car, a car
  ferry, a few cross-border stops — sit outside the main network component in the
  timetable data and read as unreachable. Zones 182 and 183 cannot be priced at
  all, because their only stops are Obersee boat piers.
- Single tickets only: no season tickets, day passes, or multi-zone boundary
  stops.

Switzerland:

- Rail only, and only stations a train calls at, since the national tariff is a
  railway tariff.
- The open fare data is **from 2023** and sits below current prices. Single
  full-fare tickets only; Halbtax is taken as half.
- Estimated fares have a median error of 6.7%, with nine in ten within 19%. The
  map labels every fare as published or estimated.
- Regional fare unions are identified but not priced; Z-Pass and operator
  supplements are not modelled.

The source portal is explicit that its data "does not constitute a sufficient
condition for ticket sales by third parties", and both pages say so too.

## Repository layout

```
crates/build/src/
  main.rs        clap subcommands
  crs.rs         LV95 -> WGS84 reprojection
  wfs.rs         zone + GTFS + OSDM fetching
  zones.rs       reproject, dissolve, simplify
  stations.rs    GTFS stops, parent collapsing, point-in-polygon, traversed zones
  graph.rs       edge set, shortcut removal, connectivity
  search.rs      multi-label Pareto search
  zoneset.rs     u64 bitmask, zone weights, the 110/120 double rule
  fares.rs       price table
  national.rs    OSDM parsing, rail graph, tariff-km calibration
  validate.rs    fixtures + label growth
data/
  fares.toml     hand-maintained ZVV prices — re-verify each December
  national/      generated national fare data + PROVENANCE.md
docs/
  index.html  national.html    the two maps
  app.js  national.js  style.css
  vendor/                      MapLibre GL JS 4.7.1, BSD-3-Clause
  data/                        generated by the pipeline, ~750 KB
notes/
  engineering-notes.md         data quirks, corrections, measurements
```

`docs/` really should be `web/` but this is for publishing on github pages.
`docs/app.js` is a direct port of `search.rs` and the two are diffed against each
other by `validate --dump`. Keep them in step. (Compiling the Rust to WASM and
deleting the JavaScript would remove the duplication, if it ever becomes
annoying.)

## Data sources and attribution

Attribution for the fare-zone data is mandatory and is shown in the site footer.

| Data | Source | Terms |
|---|---|---|
| ZVV fare zones | Kanton Zürich (OGD), GIS-ZH Nr. 348, opendata.swiss | "Reference Required" (BY) — attribution mandatory |
| Timetable (GTFS) | [opentransportdata.swiss](https://opentransportdata.swiss/) | Open data |
| National fares (OSDM offline) | [opentransportdata.swiss](https://data.opentransportdata.swiss/en/dataset/osdm-offline) | Open data; see [`data/national/PROVENANCE.md`](data/national/PROVENANCE.md) |
| Regional fare unions | SBB Tarifverbundkarte | Open data |
| Basemap | © swisstopo, `wmts.geo.admin.ch` | swisstopo terms |
| MapLibre GL JS 4.7.1 | [maplibre/maplibre-gl-js](https://github.com/maplibre/maplibre-gl-js) | BSD-3-Clause, vendored in `web/vendor/` |

ZVV prices are transcribed by hand from *Tickets und Preise* and must be
re-verified each December when the new tariff takes effect.

This project is not affiliated with ZVV, SBB, or swisstopo.

## Not built

No accounts, backend, database, real-time data, journey planning, ticket links,
multi-zone boundary stops, season tickets, day passes, or WASM. The national view
is rail-only and does not model regional fare unions, Z-Pass, or the supplements
some operators charge.

## License

Licensed under the [GNU General Public License v3.0](LICENSE).

Third-party data and assets keep their own terms — see
[Data sources and attribution](#data-sources-and-attribution).
