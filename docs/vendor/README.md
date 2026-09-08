# Vendored third-party assets

**MapLibre GL JS 4.7.1** — BSD-3-Clause, https://github.com/maplibre/maplibre-gl-js

Vendored rather than pulled from a CDN so that publishing the `web/` directory
gives a site with no third-party runtime dependency other than the swisstopo
basemap tiles. The licence header is preserved at the top of `maplibre-gl.js`.

To upgrade, replace both files with a newer release and bump the version noted
here and in the HTML comment.
