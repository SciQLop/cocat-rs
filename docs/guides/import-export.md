# Bring your catalogues in

You probably have event lists already: in tscat, in AMDA, in a spreadsheet, in a paper's
supplementary material. cocat reads the common formats and writes them back.

## Formats

| Format | Read | Write | Good for |
|---|---|---|---|
| **VOTable** (`.xml`) | yes | yes | AMDA, TOPCAT, older Python `cocat` |
| **tscat JSON** (`.json`) | yes | yes | moving from tscat / older SciQLop |
| **tscat database** | yes, from Python | – | your local tscat catalogues, all at once |
| **TFCat** (`.json`) | yes | yes | radio events with time–frequency shapes |
| **CSV** (`.csv`, `.tsv`) | yes | yes | spreadsheets, tables from papers |
| **pandas / polars DataFrame** | yes, from Python | yes, from Python | analysis in notebooks |

## Import in the browser

Use **Import…** in the project menu, or the **+** above the catalogues. You can also drop the
file on the events.

The page reads the file first and tells you what it will do, for example:

> Adds 2 catalogues and 300 events, updates 5 events (same ids)

Nothing changes until you press **Import**. The whole import is one change in the history, so
you can always restore the version before it.

Some details:

- An event with the same id as one already in the project is **updated**, not copied. So
  importing the same tscat file twice doesn't make duplicates.
- A **VOTable** always makes new catalogues.
- A **tscat JSON** file updates the catalogue with the same id.
- A **TFCat** file makes one new catalogue, named after the file's title.
- A **CSV** file makes one new catalogue, named after the file. See [CSV files](#csv-files).

Only editors can import.

## Export in the browser

Open a catalogue, then **More → Export…**, and pick the format.

To put several catalogues in one VOTable or tscat file, use **Export catalogues…** in the
project menu and tick the ones you want.

Readers can export too.

!!! warning "What each format keeps"
    - **VOTable** needs every event to carry the same attributes.
    - **tscat JSON** keeps the ids, but not the status or the comment. tscat stores the
      confidence as a rating from 1 to 10, so it comes back rounded to one decimal.
    - **TFCat** knows shapes in frequency, wavelength, energy or wavenumber. Events without a
      shape become rectangles from start to stop.
    - A **smart catalogue** is exported with the events it holds now, without its rule.

## Moving from tscat

If you used tscat (or the catalogue panel of an older SciQLop), your catalogues are in a local
database on your computer. One call brings them all into a project:

```sh
pip install 'cocat-rs[tscat]'
```

```python
import cocat_rs as cc
from cocat_rs import tscat

project = cc.open()           # My catalogues, or any project you can edit
tscat.import_database(project)
project.save()
```

What happens:

- ids are kept, so running it again updates instead of duplicating;
- a tscat **rating** (1 to 10) becomes a **confidence** (0 to 1);
- removed items go to the **Trash**;
- **dynamic catalogues** become **smart catalogues**, with their predicate as the rule.

If you only have tscat's JSON export, you don't need tscat installed:

```python
tscat.import_json(project, "export.json")
tscat.export_json(project, [catalogue], "back_to_tscat.json")
```

## From AMDA

Export your catalogue from AMDA as a VOTable, then import it in the browser or from Python:

```python
project.import_votable("my_amda_catalogue.xml")
```

AMDA's shared catalogues may already be on your server, under **Public catalogues → AMDA**.
See [Community catalogues](public-catalogues.md).

## CSV files

A CSV file is a table: one row per event, one column per field. Spreadsheets save it, papers
publish it, and most tools read it.

The only columns cocat needs are **start** and **stop** (or **end**). Some columns have a
meaning of their own:

| Column | Becomes | Example |
|---|---|---|
| `start`, `stop` (or `end`) | the time interval, in UTC | `2020-01-01 10:00:00`, `2020-01-01T10:00:00Z` |
| `tags` | tags, separated by `;` or `,` | `shock; foot` |
| `status` | the status | `reviewed` |
| `confidence` | the confidence, 0 to 1 | `0.8` |
| `author` | the author | `ana@lab.org` |
| `comment` | the comment | `data gap at 12:03` |
| `uuid` | the event's id: an event with that id is updated | (written by the export) |

Every other column becomes an **attribute**. A cell that reads as a number becomes a number,
`true` and `false` become yes/no values, and the rest stays text. Empty cells are left out.

The separator can be a comma, a semicolon (as European spreadsheets save) or a tab: cocat
picks the one the header uses. Column names don't care about capitals.

A row with a time cocat can't read stops the import, and the message names the row. A status
or a confidence it can't read is left out, with a warning.

```python
catalogue = project.import_csv("shocks_table2.csv")          # named after the file
catalogue = project.import_csv("export.csv", name="Shocks")  # or named as you like
cc.export_csv([catalogue], "shocks.csv")
```

!!! tip "Edit in a spreadsheet, then import back"
    The CSV export writes each event's `uuid`. Open the file in a spreadsheet, fix a few
    values, save it as CSV and import it: the events with those ids are updated, not copied.

## DataFrames: pandas and polars

In a notebook, a catalogue becomes a DataFrame in one call, and a DataFrame becomes a
catalogue in one call:

```python
frame = catalogue.to_pandas()      # or catalogue.to_polars()
```

You get one row per event, in time order. The columns are `uuid`, `start`, `stop`, `tags`,
`status`, `confidence`, `author`, `comment`, then one column per attribute.

To go the other way, give the DataFrame and a name:

```python
import pandas as pd

table = pd.read_csv("smith2021_table2.csv", parse_dates=["start", "stop"])
catalogue = project.import_dataframe(table, name="Smith et al. 2021, table 2")
```

It reads the same columns as a CSV file: `start` and `stop` are needed; the known columns
become fields, the others attributes; empty cells (`NaN`, `None`) are left out.

<figure>
--8<-- "dataframe.svg"
<figcaption>A round trip through pandas: the uuid column makes the new catalogue list the same events, now updated.</figcaption>
</figure>

The `uuid` column makes a round trip work: compute a new column in pandas, import the frame,
and the same events get the new attribute.

## From Python, in general

```python
project.import_votable("catalogue.xml")
project.import_tfcat("bursts.json")

project.import_csv("table.csv")
project.import_dataframe(frame, name="From pandas")

cc.export_votable([catalogue], "catalogue.xml")
cc.export_tfcat(catalogue, "bursts.json")
cc.export_csv([catalogue], "catalogue.csv")
frame = catalogue.to_pandas()
```
