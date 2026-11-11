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
| **CSV** (`.csv`) | – | yes | spreadsheets, supplementary material |

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

## From a spreadsheet or a paper's table

There is no CSV import. A few lines of Python do it, and let you decide what each column
means:

```python
import pandas as pd
import cocat_rs as cc

table = pd.read_csv("shocks_table2.csv", parse_dates=["start", "stop"])

project = cc.open("Shocks at L1")
catalogue = project.create_catalogue(name="Smith et al. 2021, table 2")
with project.transaction():
    catalogue.add_events([
        project.create_event(
            start=row.start.isoformat() + "Z",
            stop=row.stop.isoformat() + "Z",
            tags=["shock"],
            attributes={"theta_bn": row.theta_bn, "mach": row.mach},
        )
        for row in table.itertuples()
    ])
project.save()
```

## From Python, in general

```python
project.import_votable("catalogue.xml")
project.import_tfcat("bursts.json")

cc.export_votable([catalogue], "catalogue.xml")
cc.export_tfcat(catalogue, "bursts.json")
```
