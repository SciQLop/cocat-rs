# Shared workspaces: a folder kept in sync through cocat-rs, notebooks co-edited live

## Why

Catalogues already sync between people through cocat-rs: offline-first, merged edit by
edit, with projects, roles, groups and invitations deciding who sees what. Scientists
also want to share the rest of their work: the notebooks, scripts and notes around those
catalogues. Today they mail files or push to git, and lose each other's edits.

JupyterLab already knows how to co-edit a notebook live (its real-time collaboration, on
the same merge library cocat uses). We use that for the editing, and cocat-rs as the
meeting point. Three gains:
- people see each other's edits, cursors and outputs as they happen, without reloading;
- an AI agent edits the same live notebook as a collaborator, under its own name, instead
  of rewriting a file behind JupyterLab's back;
- access follows the workspace's people and roles, as for catalogues.

Earlier notes: SciQLop memory `collab-server-full-workspace-design.md` (July 2026),
issues SciQLop/SciQLop#132 and SciQLop/cocat#110.

## Goals

1. A **shared workspace** is an ordinary SciQLop workspace (the folder SciQLop already
   opens: notebooks, scripts, `.sciqlop` package list, its own `.venv/`) linked to a
   workspace on cocat-rs. Joining one creates a normal local SciQLop workspace that fills
   from cocat-rs. Nothing changes in how SciQLop workspaces work. Everything in the folder
   syncs, except files the exclusion rules leave out or that are too big; `.sciqlop`
   syncs, so members get the same packages, and `.venv/` never does.
2. **Notebooks** and **text files** merge edit by edit; opened in JupyterLab, they are
   co-edited live with cursors and presence.
3. **Other files** (figures, PDFs, small images) sync as whole files.
4. **Offline first.** Every member keeps a full local copy and works as usual; changes
   merge on reconnect. The folder on disk stays a normal folder (git and other tools see
   ordinary files).
5. **AI agents** join a notebook as collaborators.
6. **Easy to unify later** with catalogue projects (see "One model underneath").

## Non-goals (later)

- SciQLop's own state (plot panels, products, time range): personal for now.
- Sharing a workspace does not share the catalogue projects its notebooks use; the two
  stay shared separately.
- Data files: excluded by default (people fetch data themselves).
- Live "watch party" sessions or shared kernels: each person runs their own kernel; what
  is shared is the notebook, outputs included.

## Anti-goals (a failure even if it works)

- A colleague connecting **into** someone's machine. Every client dials out to cocat-rs.
- Losing work silently: a merge that duplicates cells, a file deleted because a rule
  changed, an overwritten file version that cannot be restored.
- A workspace that only works online.

## Decisions taken (the user's)

- cocat-rs is the server; no separate collaboration server.
- JupyterLab real-time collaboration is the editing layer for notebooks and text files.
- Sync everything except excluded extensions and files over a size limit. Data formats are
  excluded by default.
- Any excluded file under the size limit can be shared on purpose ("Share this file
  anyway"), e.g. a calibration table that cannot be fetched again.
- Notebook outputs are shared as they are, inside the notebook (decided 2026-11-05;
  storage growth accepted until history compaction exists).
- Workspaces and catalogue projects are separate shareable things for now, built so they
  can be unified later.

## One model underneath (so unifying later is a setting)

Today a cocat-rs project is one room holding one document (the catalogues). This becomes:

- A **project** holds **documents**. A document has an id, a kind (`catalogues`,
  `manifest`, `notebook`, `text`) and its own update file and room thread.
- A project's **kind** (`catalogues` or `workspace`) only decides which documents it may
  hold and how the web app and SciQLop present it. Members, roles, groups, invitations,
  history, trash, the personal-project rule and revocation are the project's, shared by
  all its documents.
- Unifying later = letting one project hold both a catalogues document and a manifest.
  No data moves; no second ACL to merge.

Server consequences:
- Socket: `/room/{project}/{doc}`. `/room/{project}` stays the catalogues document, so
  today's clients keep working.
- Storage: `update_dir/{project}/{doc}.y`; a catalogue project's existing file stays where
  it is (its path is the catalogues document's path).
- *(Designer's call:)* documents are created through the API
  (`POST /api/rooms/{id}/documents` with a kind, by an editor), recorded in a `document`
  table, and a socket to an unknown document id is refused. That is where quotas and
  abuse limits apply; the server never reads the manifest.
- The room registry is keyed by (project, document). Revoking someone, deleting or
  trashing the project acts on every document of the project. The memory budget counts
  every open document.
- History records per document; a version still belongs to the project (one timeline per
  project, entries naming the document).

## What a workspace holds

1. **The manifest document**: one map, path → entry. An entry is
   `{kind: notebook | text | file, doc: <document id> | hash: <sha256>, size, by, at}`,
   where `at` is an integer, milliseconds since the Unix epoch.
   A path is a key, so each path is last-write-wins for free; a rename is a manifest edit
   (the document id stays, so a renamed notebook keeps its history and live sessions).
   The manifest also holds the workspace's **sharing rules** (below).
2. **One document per notebook or text file**, in the jupyter_ydoc schemas (`YNotebook`,
   `YUnicode`) so JupyterLab, the kernel side and AI tools read them as they are.
3. **Whole files** as content-addressed blobs:
   - `PUT/GET /api/rooms/{id}/blobs/{sha256}`; the server checks the hash on upload;
   - viewers read, editors write;
   - a hard size cap per file, enforced by the server whatever the client sends;
   - the owner pays (the user's decision): blobs count toward the workspace owner's
     existing storage quota, with the project's update files, as room files do today; no
     separate workspace quota. Members cannot run the owner out of space by surprise
     because of the per-file cap and the excluded data formats; an owner short of space
     hands ownership over or asks the admin for more;
   - a blob is swept only when no manifest entry and no history version within the
     project's history retention refers to it, so an overwritten version stays restorable.
   - Blob bytes never go through the merge documents.

### Sharing rules

- `.gitignore` syntax, stored in the manifest so every member applies the same rules.
- Defaults: version control and caches (`.git/`, `__pycache__/`, `.ipynb_checkpoints/`,
  `.venv/`, build folders, temporary files), Speasy's cache, and data formats (`.cdf`,
  `.h5`, `.hdf5`, `.nc`, `.fits`, `.npy`, `.npz`, `.parquet`, `.pkl`).
- **Forced files**: "Share this file anyway" adds an exception (`!path`) for that file;
  "Stop sharing" removes it. Copies people already have stay on their disks. Forced files
  carry a mark in SciQLop's file list.
- A file over the size cap stays local, and SciQLop says why ("too big to share").
- Changing a rule never deletes anyone's local file.

### Conflicts

- Notebooks and text: merged edit by edit; no conflict to resolve.
- Whole files changed by two people while apart: the last writer wins (the user's
  decision). The version it replaced stays in the file's history and can be restored, so
  nothing is lost, only replaced.
- Delete versus edit: the edit wins and the file comes back. *(Designer's call:)* this is
  the client's job. A bridge that pushes edits to a document whose path is no longer in
  the manifest puts the entry back at its last path. The server keeps a document that no
  manifest names for as long as the project's trash keeps things, then sweeps it.

## SciQLop side (jupyqt and SciQLop)

Facts (read 2026-11-10): jupyqt runs jupyverse in-process; it enables every installed
`jupyverse.modules` entry point and already replaces the contents module with its own.
No collaboration piece is installed (no fps-yjs/fps-yrooms, no `@jupyter/collaboration`
or `@jupyter/docprovider` front-end extensions), and there is no AI integration.
pycrdt pins yrs 0.28.1, cocat-rs uses yrs 0.28: same encoding.

Architecture: **the local Jupyter server mirrors each room to cocat-rs.**
- jupyverse's collaboration (the `collaboration` extra, fps-yrooms) runs locally; the
  front end is stock JupyterLab collaboration (`collaborative: true`).
- **Where the code lives** (the user's direction: keep cocat out of jupyqt). jupyqt gains
  only generic pieces: turning collaboration on, and a hook to wrap or replace the room
  factory (a "room mirror" interface with no cocat in it). SciQLop (or a small package it
  installs) holds the cocat side: the bridge, the manifest, the folder watcher, sign-in.
- The room factory (fps-yrooms registers a replaceable
  `YRoomFactory`): each local room also runs a **bridge** to the matching cocat-rs
  document, with reconnect, echo filtering by transaction origin, and the user's cocat
  token (as cocat_rs's Python client already sends).
- **First load order** (the known trap): a room is filled from cocat-rs or from its local
  update store, which share history, *before* it ever looks at the file on disk; then the
  file is compared and applied as a diff. Building a fresh document from the file and
  merging it with a remote copy duplicates every cell.
- Rooms of a shared workspace stay open while SciQLop runs (pinned), so background sync
  and agents keep working with no tab open.
- Room ids: JupyterLab's file ids are local to each machine. The bridge maps a path to the
  manifest's document id; the manifest is the only shared naming.
- Files not open in JupyterLab: a folder watcher keeps disk and workspace in step. A text
  file changed on disk (e.g. in another editor) is applied as a diff, one edit; a whole
  file is hashed and uploaded.
- Offline: the local room, its update store and the file keep working; the bridge resyncs
  on reconnect.
- The local files are written back from the shared documents, so the kernel, git and AI
  tools see what people see.

Alternative rejected: the front end talking straight to cocat-rs. It loses offline work
and the files on disk, and the browser would not send the cocat cookie across sites.

## Presence

cocat-rs relays presence today but keeps only `{user, editing}` per client, with a 4 KB
frame cap. JupyterLab needs its cursors and selections and the user's display name. For
`notebook` and `text` documents the server keeps JupyterLab's fields (`user` still forced
to the authenticated account, cursors, selections), within a size cap. Catalogue
documents keep today's rule; public catalogues still relay nothing.

## AI agents

No setup and no account (the user's decision). An agent edits the user's notebooks
locally, through pycrdt on the local room (in-process, or as a client of the local
collaboration room), exactly like JupyterLab does. Its edits reach cocat-rs through the
user's bridge, as the user's edits. People see them live; undo works as for any edit.
Existing tools work this way (jupyter-mcp-server's notebook client joins the
collaboration room); jupyter-ai's own tools look up rooms through Jupyter Server and would
need an adapter on jupyverse.

## Restore

Restoring puts back a single file (notebook, text or whole file) as it was at a version
(the user's decision). Restoring a whole workspace to a moment is later.

## The web app: a workspace viewer

- A workspace's page shows its file tree (names, kinds, sizes, who changed what, when),
  and sharing, members, groups, invitations and trash as for catalogue projects.
- Opening a file shows it, **live**: the web app joins the file's document like any
  client (it already syncs catalogue documents with yrs), so a colleague's edits, new
  cells and fresh outputs appear as they happen.
  - Notebooks render as a notebook: markdown cells rendered, code cells with syntax
    colouring, outputs (text, errors, PNG/JPEG/SVG images; HTML outputs sanitised or shown
    as text).
  - Text files show with syntax colouring by extension; whole files show as a download,
    images inline.
  - Each file has its history, and Restore for editors.
- Viewing only: editing stays in SciQLop/JupyterLab for now.

## Left to the builder

- The exact manifest field names and the history entry format per document.
- The size cap default (suggestion: 20 MB per file).
- The blob sweep grace period.
- The order of the work. A suggestion: (1) server: documents per project, sockets,
  storage, registry, presence for jupyter documents; (2) manifest and blobs; (3) the
  jupyqt bridge and front-end extensions; (4) the folder watcher; (5) AI agents; (6) the
  web app's workspace viewer.

## Open risks

- jupyverse's room handler does not answer JupyterLab's explicit "save" message (seen in
  source, not tested): Ctrl+S may show no confirmation. Check early.
- `@jupyter/collaboration` versions against jupyqt's JupyterLab 4.5.
- Notebook outputs grow the update files with every run until history compaction exists.
