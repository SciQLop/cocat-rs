# TFCat import and export

## Why

SciQLop plots radio spectra, and radio features are catalogued as time–frequency shapes in
TFCat (Cecconi et al., doi:10.25935/6068-8528: a GeoJSON profile with time and frequency
as the two coordinates). Events already hold such a shape (`shape`: a list of
`{time, frequency}` vertices, which sets start/stop). What is missing is reading and writing
TFCat files, so radio catalogues can come in and go out.

## Where

In `cocat-core` (`tfcat.rs`, beside `votable.rs` and `tscat_json.rs`), so Python, the
server, the web app's Import/Export and SciQLop share one implementation. Parse and write
with well-established crates (serde_json; the `geojson` crate if it fits the geometry types
without fighting the non-geographic coordinates), not by hand.

## Import: one TFCat file → one catalogue

- Catalogue name: the collection's `properties.title` if any, else the file name without
  extension (the caller passes it). Collection `properties` → catalogue attributes.
- Each feature → one event. Feature `id` → `source_id`. Feature `properties` → event
  attributes (types registered as usual).
- Geometry:
  - `Polygon`: its outer ring → `shape` (ring closure point dropped). Holes are dropped with
    one warning for the file.
  - Any other geometry (Point, MultiPoint, LineString, MultiLineString, MultiPolygon,
    GeometryCollection): start/stop = its time span; the geometry is kept verbatim (as JSON
    text) in the event attribute `tfcat_geometry` so export writes it back unchanged. A
    MultiPolygon with one polygon counts as a Polygon.
- Time: the CRS time coordinates (`time_coords_id`: unix, jd, mjd, mjd_cnes, mjd_nasa,
  cdf_tt2000, and the custom `time_coords` form with unit / time_origin / time_scale) are
  converted to our timestamps. Frequencies are stored as given, in the file's spectral unit.
- The `crs` and `fields` objects are kept on the catalogue (attributes `tfcat_crs`,
  `tfcat_fields`, JSON text), so export restores them. A file without a `crs` is refused with
  a plain error.

## Export: one catalogue → one TFCat file

- A `FeatureCollection` with the catalogue's `tfcat_crs` (else a default CRS: unix time,
  frequency in Hz), `tfcat_fields` merged with fields for the attributes the events carry,
  and the catalogue's attributes as `properties` (name as `title`).
- An event with `tfcat_geometry` → that geometry; with a `shape` → a closed `Polygon`; with
  neither → a rectangle from start to stop over a frequency range: the caller's
  `frequency_range`, else the range covered by the catalogue's shaped events. With neither,
  export refuses in plain words ("These events have no frequency: give a frequency range").
- Event attributes → feature properties; `source_id` → feature `id` (else the event uuid).
- Times written in the CRS's time coordinates.

## Python

- `project.import_tfcat(path, name=None)` → the new catalogue (as `import_votable` does for
  its file, adapted to one catalogue).
- `cocat_rs.export_tfcat(catalogue, path, frequency_range=None)` and a function returning
  the JSON text, mirroring `export_votable`.
- README: a short TFCat paragraph next to VOTable.

## Web app

- Import (project menu "Import…" and drop): a `.json` that is a TFCat FeatureCollection
  imports as TFCat; tscat JSON keeps working (tell them apart by content).
- Export ("Export catalogues…" and the catalogue's own export): TFCat as a format next to the
  existing ones; one file per catalogue. A catalogue whose events have no frequency asks
  for a frequency range (two number inputs, in the CRS unit) rather than failing.

## Tests

Round trip (import → export → import gives the same events, shapes, attributes, CRS);
each time coordinate system; each geometry type; a real published TFCat file (the MASER
catalogues are CC-BY; keep it small, credit it next to the file); refusal without a CRS;
export of shape-less events with and without a range.

## Non-goals

Unit conversion of frequencies; validating against the JSON schema beyond what parsing
needs; drawing shapes in the web app's timeline.
