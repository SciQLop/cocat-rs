# SciQLop

[SciQLop](https://sciqlop.github.io) is where most events are born: you look at the data, see
something, and mark the interval. With cocat, the intervals you mark land in a shared
catalogue, and your colleagues see them appear in their browser or their own SciQLop.

## Connect

1. In SciQLop, open the cocat plugin's settings.
2. Enter the server address. Your team gives it to you; the web app's **Help** page shows it
   too.
3. Sign in when SciQLop asks.

Your projects then appear in SciQLop.

## Mark events

Pick a catalogue and mark intervals on the plots. Each interval is an event of that catalogue.
Its start and stop follow what you drew.

Edits go both ways, live:

- an event you mark in SciQLop appears in the browser within a second;
- an event a colleague fixes in the browser moves on your plot.

SciQLop keeps a copy of your projects on your computer. You can mark events offline, and they
are sent when you're back online.

## Share a workspace

A project can also hold the files of a SciQLop workspace: notebooks, scripts, notes, figures.
Share a workspace from SciQLop into a project, and everyone in the project can open it.
Notebooks are edited together, live, like the catalogues.

In the browser, the files show under **Files**, below the catalogues. Notebooks show with
their outputs and plots. Each file has its own history.

!!! note "Data files stay home"
    Data files such as CDF or HDF5 are not shared by default: everyone fetches the data for
    themselves. A file that can't be fetched again, like a calibration table, can be shared on
    purpose from SciQLop.

## A typical day

1. Open the shared project in SciQLop.
2. Open the smart catalogue `To check` (for example `status:preliminary`).
3. Go from event to event, look at the data, set the status to `confirmed` or `rejected`, fix
   the start and stop.
4. Your colleague does the same on another part of the list. You never step on each other.
5. At the end of the week, press **Save version** in the browser: `week 12 checked`.
