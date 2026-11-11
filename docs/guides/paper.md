# Catalogues for a paper

A paper says "we used 412 bow shock crossings". A year later, a referee or a reader asks:
which 412? By then the catalogue has moved on. People fixed times, added events, rejected
some.

cocat keeps every version, so the exact list behind the paper can always be found again.
You only need to mark it.

## 1. Name the version

When the list is the one you analysed, press **History**, then **Save version**.
Give it a name and a note:

- name: `paper v1`
- note: `as submitted to JGR, 2026-03-02`

From Python:

```python
project.commit(name="paper v1", message="as submitted to JGR, 2026-03-02")
```

Saved versions carry a mark in the history list. You can also name an older version after the
fact: pick it and use **Name this version**.

Do this again at each step: `paper v2 (after review)`, `paper accepted`.

## 2. Keep working

Naming a version freezes nothing. The team goes on editing the project as before. The named
version stays exactly as it was, in the history.

## 3. Get that version back

To **look** at it: open History and pick the version. You see what it changed compared with
the one before.

To **work** on it without touching the project: pick it and press **Make a copy from this
version**. You get a new project of your own, holding the catalogues as they were. Anyone in
the project can do this, viewers included.

From Python, open it read-only:

```python
past = cc.Project.at(cc.default_client().server, cc.default_client().session,
                     project_id, version_id)
paper = past.catalogue("Final")
```

To make the project itself go back: **Restore this version**. Nothing is lost. The restore is
a new version, and the events made since go to the trash.

## 4. Export the file for the journal

Most journals and data repositories want a file. Open the catalogue, then **More →
Export…**:

- **VOTable** (`.xml`): the astronomy table standard. TOPCAT and AMDA read it. Good for a data
  repository like Zenodo.
- **CSV** (`.csv`): opens in any spreadsheet. Good as supplementary material.
- **TFCat** (`.json`): for radio events with time–frequency shapes.

Export from a copy of the named version (step 3), so the file is exactly that version.

!!! tip "Say where it came from"
    In the paper's data availability section, give the file's DOI (from Zenodo, for
    instance), the server, the project, and the version name. Anyone with access can then open
    that version.

## 5. Make it public

To let everyone on the server use your catalogue, open it and choose **More → Submit to Public
catalogues…**. A curator reviews it. Once accepted, it appears under **Public catalogues**
with your name, for every account to read and copy.

See [Community catalogues](public-catalogues.md).
