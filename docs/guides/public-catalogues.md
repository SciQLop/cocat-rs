# Community catalogues

You rarely start from nothing. Someone already listed the CMEs, the flares, the ICMEs. cocat
puts those lists next to your own, under **Public catalogues**.

## What is there

Open **Public catalogues** on the left, under your catalogues. They are sorted in folders, such
as `AMDA / Solar wind`. The search box finds a catalogue by its name, folder, source or
description, best answer first: a word in the name counts more than the same word in a
description, and a rare word more than a common one, as in SciQLop's product search. Paste a
DOI to see whether a paper's list is already there.

Some are kept by **curators**, people the server's administrators trust to accept and edit
them.

Others **follow a source** published elsewhere. The server reads the source again on a
schedule, and the catalogue then holds exactly what the source says. Depending on the server,
you may find:

- NASA's **DONKI** space weather database: flares, CMEs, solar energetic particles,
  interplanetary shocks, geomagnetic storms…
- the **CDAW** SOHO/LASCO CME catalogue, and Wind/STEREO WAVES type II bursts;
- **AMDA**'s shared catalogues;
- **HELIO4CAST** ICMECAT, one catalogue per spacecraft, and its companion lists;
- **Solar Orbiter STIX** flares;
- the SIDC's automatic **CACTus** CME detections;
- the Cluster **GRMB** and Solar Orbiter **PMOD** lists;
- **ONERA**'s ICME, sheath and SIR lists from OMNI, their radiation belt electron
  enhancements back to 1868, and the detections of Nguyen et al. (2025).
- **Published lists**, under *Literature*: event lists deposited with papers or by mission
  teams. Boundary crossings at Mercury, Venus, Earth, Mars, Jupiter and Saturn; the SHARP and
  Helsinki IPShocks shock databases and the CfA's (Wind, ACE, DSCOVR); Wind AKR bursts; the
  small-scale flux ropes of Hu et al. (Wind, ACE, Ulysses, Parker Solar Probe); the CDAWeb
  event lists (ISTP Solar Wind Catalog, bow shock crossings since 1973, magnetopause crossings
  since 1963, current sheet exhausts, the 1859 aurora sightings); HELCATS HIGeoCAT and LINKCAT;
  MMS burst selections and magnetopause crossings; Lan Jian's STEREO ICME, SIR, shock and SEP
  lists; the ICME multi-catalogue of Larrodera & Temmer, which says which source catalogue each
  ICME comes from; LLAMAICE (Kay et al. 2026), every near-Earth CME candidate with the
  team's verdict and boundaries, and the six catalogues it collates side by side;
  the intense storms of Ji et al. (2012); Solar Orbiter's mirror mode storms and reconnection
  exhausts (Fargette et al. 2023); Cassini's long-standing reconnection at Saturn (Guo et al.
  2019); and Parker Solar Probe's heliospheric current sheet crossings (Fargette et al. 2026).
- **VizieR**, under the same *Literature* folders: 27 event tables published with papers and
  kept by CDS, for instance CME source regions, halo and limb CMEs, EIT waves, white-light and
  Lyman-α flares, ICME heavy ions, Forbush decreases, ³He-rich periods, extremely slow solar
  wind, and pickup-ion waves at the Voyagers.
  Each says whom to cite and under which licence.

Such a catalogue says where it comes from under its name, for instance "From DONKI (NASA
CCMC), refreshed 2026-11-11", with a link to the source. When the source corrects an event,
the catalogue follows, and the change shows in its **History**.

<figure>
--8<-- "public.svg"
<figcaption>Where public catalogues come from, and how your copy and your submissions relate to them.</figcaption>
</figure>

## Use one

You read a public catalogue like any other: table, timeline, statistics, history, search,
export. You can't change it.

To work with it, press **Copy to a project…**. The catalogue and its events go into one of
your projects, or a new one. Your copy is yours: add attributes, reject events, merge it with
your own list. The public catalogue stays as it is.

From Python:

```python
import cocat_rs as cc

for entry in cc.public_catalogues():
    print(entry["id"], entry["folder"], entry["name"], entry["count"])

copy = cc.copy_public_catalogue("public.donki.cme")      # an id from the list above
project = cc.open(copy["project_id"])
cmes = project.catalogue(copy["catalogue"])
```

## A worked example: which of my events match a CME?

You have a catalogue of ICME-driven shocks at 1 AU. You want to know which ones follow a CME
seen by LASCO.

1. Copy the CDAW LASCO CME list into the project that holds your shocks.
2. Pair them in Python:

```python
from datetime import timedelta

mine = next(info for info in cc.projects() if info.name == "Shocks at L1")
copy = cc.copy_public_catalogue("public.cdaw.lasco", mine.id)

project = cc.open(mine.id)
shocks = project.catalogue("ICME shocks")
cmes = project.catalogue(copy["catalogue"])      # "SOHO LASCO CMEs", your copy

for shock in shocks.events:
    candidates = [
        cme for cme in cmes.events
        if timedelta(days=1) <= shock.start - cme.start <= timedelta(days=5)
    ]
    shock.set_attributes(n_cme_candidates=len(candidates))
```

3. Back in the browser, search `#n_cme_candidates:>0` and save it as a smart catalogue.

## Offer your own

Made a catalogue others could use? Open it and choose **Catalogue → Submit to Public
catalogues…**. Give it a title, a folder and a description that says how it was made. Add a
note for the curators if you like.

The curators get the catalogue as it was when you pressed Submit. What you change afterwards
stays in your project. Submit again to send a newer version.

**My submissions**, on the left of the public catalogues, shows each one you sent: waiting,
accepted or declined, with the curator's note.

## Fix a public catalogue

Found a wrong time in a curated catalogue?

1. Open it and choose **Propose a change…**. It is copied into one of your projects.
2. Fix the copy.
3. Choose **Catalogue → Submit as an update…** on the copy, with a note saying what you changed.

A curator sees exactly what your update adds, changes and removes, and accepts or declines it.

Catalogues that follow a source can't be changed this way: the source is the truth. Copy it
and fix your copy, and tell the source's maintainers.
