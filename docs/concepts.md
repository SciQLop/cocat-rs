# Key ideas

cocat has few ideas. This page explains each one in a few lines. The [glossary](#glossary)
at the end lists them all.

## Events

An **event** is a time interval: a start and a stop, in UTC. Around it, you can add:

| Field | What it is for | Example |
|---|---|---|
| tags | short words to sort events | `shock`, `quasi-perpendicular` |
| status | how far the event has been checked | preliminary → confirmed → reviewed → final, or rejected |
| confidence | how sure you are, from 0 to 1 | `0.8` |
| attributes | any named value you need | `spacecraft = "MMS1"`, `density = 12.5` |
| comment | free text | "data gap at 12:03, stop is approximate" |
| start/stop error | how uncertain the times are, in seconds | `30` |
| shape | a region in time and frequency (or energy…) | the outline of a radio burst on a spectrogram |

Attributes are where your science goes. A catalogue of CMEs may carry `speed` and `width`; a
catalogue of shocks may carry `theta_bn` and `mach`. cocat remembers each attribute's type
across the project, and you can give it a unit and a description.

## Catalogues

A **catalogue** is a named list of events: "Bow shock crossings", "Type III bursts 2022",
"Candidate flux ropes".

One event can belong to **several catalogues** of the same project. Put a shock in "All shocks"
and in "Shocks for the paper": it is the same event, so a correction shows in both.

<figure>
--8<-- "catalogues.svg"
<figcaption>Shock B is listed by both catalogues. It is one event: fix it once and both show the fix.</figcaption>
</figure>

## Smart catalogues

A **smart catalogue** has a rule instead of a fixed list. "Every shock with confidence above
0.8". "Everything Alice added this week". "Events that are not reviewed yet". It updates itself
as people edit the project.

The easy way to make one: type a search, then press **Save as new smart catalogue…**.
See [Search](reference/search.md).

To keep what a smart catalogue holds today, use **More → Freeze…**. It makes a normal
catalogue you can edit.

## Projects

A **project** holds catalogues. It is the unit you share: everyone in a project sees all its
catalogues.

- Your account starts with **My catalogues**, a private project.
- Make one project per team, study or class, and share it with the people involved.
- To share only some catalogues, put them in a project of their own.

A project can also hold the files of a SciQLop workspace: notebooks, scripts, figures.
They show in the browser under **Files**.

!!! note "Room = project"
    The server API and the command-line tools still say *room*. It is the same thing as a
    project.

## Roles

Everyone in a project has a role:

| Role | Can |
|---|---|
| **Viewer** | see everything, export, make a private copy of any version |
| **Editor** | also add, change and delete events and catalogues, restore versions, and (unless the owner turns it off) invite people |
| **Owner** | all of that, plus delete the project and hand it over. One per project |

A **group** is a set of people you share many projects with: a lab, a team, a class.
Give a group a role in a project, and every member gets it, including people who join later.

## History and versions

The server keeps **every version** of a project. A new version starts when someone pauses for
five minutes, after an hour of work, when someone else starts editing, or when someone presses
**Save version**.

Give important versions a name, like `paper v1` or `submitted to JGR`. You can:

- look at what any version changed;
- restore the whole project, or just one catalogue, to a version;
- make a private copy of a version to try something without touching the original.

Deleting puts things in the **Trash**. From there you can restore them. Only
**Delete for good** cannot be undone.

## Public catalogues

**Public catalogues** are open to every account on the server. Some are kept by curators.
Others follow a published source automatically: NASA's DONKI, the CDAW CME list, Solar Orbiter
STIX flares, HELIO4CAST ICMEs, AMDA's shared lists and more.

You can read them, copy them into your projects, and propose your own.
See [Community catalogues](guides/public-catalogues.md).

## Rounds

A **selection round** is a structured way for several people to choose events together.
Reviewers give a verdict on each candidate (accept, reject, unsure). A decider keeps one final
catalogue. Use it to check a detection pipeline, to pick intervals for a downlink, or to label
data as a class. See [Review rounds](guides/rounds.md).

## Working offline

The Python package and SciQLop keep a copy of each project on your computer. You can keep
working on a plane or when the server is down. Your changes are sent when the connection comes
back, and they merge with what others did meanwhile. Nobody's work overwrites anybody else's.

<figure>
--8<-- "offline.svg"
<figcaption>Changes made offline are sent on reconnection. Both sides end with the same project.</figcaption>
</figure>

!!! info "How can edits never conflict?"
    cocat stores projects as a CRDT, a data structure built so that edits made apart always
    merge the same way. Two people changing two different events: both changes are kept. Two
    people changing the same field at the same moment: one value wins, the same one for
    everybody.

## Glossary

Attribute
:   A named value on an event, like `density = 12.5`.

Catalogue
:   A named list of events.

Confidence
:   A number from 0 to 1 saying how sure you are about an event.

Curator
:   Someone who keeps the public catalogues: accepts submissions, edits curated catalogues.

Event
:   A time interval with a start, a stop and anything you want to say about it.

Group
:   A set of people you share projects with.

Link
:   A bookmark in My catalogues to a catalogue of another project. Nothing is copied.

Project
:   A box of catalogues that you share as a whole. Also called a *room* by the API.

Public catalogue
:   A catalogue every account on the server can read.

Round
:   A structured review or selection, ending with one final catalogue.

Smart catalogue
:   A catalogue with a rule that picks its events, updated live.

Status
:   Where an event is in your checking: preliminary, confirmed, reviewed, final or rejected.

Version
:   A saved state of a project in its history.
