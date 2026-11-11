# Python and Jupyter

The `cocat-rs` package lets a notebook or a script read and write the same catalogues you see
in the browser. Typical uses:

- your detection code writes its events straight into a shared catalogue;
- you load a catalogue into pandas for statistics or superposed epoch analysis;
- you loop over events to fetch and plot the data around each one.

## Install

```sh
pip install cocat-rs
```

It works with Python 3.10 and newer, on Linux, macOS and Windows. Inside Python the package is
called `cocat_rs`. Everyone imports it as `cc`:

```python
import cocat_rs as cc
```

## Sign in once

```python
cc.login()
```

This prints a link and a short code, and opens your browser when there is one. Sign in on that
page, check the code matches, and press **Allow**.

You only do this once per computer. The sign-in is kept in your system's keyring, so the next
notebook is already signed in.

!!! tip "JupyterHub, SSH, remote machines"
    The link can be opened on any device: your laptop, your phone. So `cc.login()` works the
    same in a JupyterHub notebook or over SSH.

!!! note "Another server"
    By default `cc.login()` talks to the SciQLop server. For your lab's server, pass its
    address: `cc.login("https://cocat.my-lab.org")`, or set the `COCAT_SERVER` environment
    variable.

To see which apps are signed in to your account, open **Account** in the browser. Revoke one
you don't use any more, or one on a lost laptop.

## Open a project

```python
project = cc.open()                  # My catalogues
project = cc.open("Bow shocks MMS")  # another project, by name (or by id)
cc.projects()                        # the projects you can open, with your role
```

`cc.open` waits until your copy is in step with the server. The copy is kept on your disk, so
the project opens even when the server is out of reach, and your changes go out when it comes
back.

## Read a catalogue

```python
shocks = project.catalogue("Bow shock crossings")   # by name or uuid

for event in sorted(shocks.events, key=lambda e: e.start):
    print(event.start, event.stop, event.tags, event.attributes)
```

Each event has `start`, `stop` (Python `datetime`, UTC), `tags`, `status`, `confidence`,
`attributes`, `comment`, `author` and `uuid`.

### Into pandas

```python
import pandas as pd

df = pd.DataFrame(
    {"start": e.start, "stop": e.stop, "confidence": e.confidence, **e.attributes}
    for e in shocks.events
).sort_values("start")

df["duration"] = df["stop"] - df["start"]
df.describe()
```

### Fetch the data around each event

cocat doesn't hold measurements, only the intervals. Pair it with a data library such as
[Speasy](https://github.com/SciQLop/speasy):

```python
import speasy as spz
from datetime import timedelta

margin = timedelta(minutes=10)
for event in shocks.events:
    b = spz.get_data("amda/imf", event.start - margin, event.stop + margin)
    ...  # plot it, fit it, measure it
```

## Write events

```python
candidates = project.create_catalogue(name="Shock candidates (detector v2)")

event = project.create_event(
    start="2019-12-01T10:15:00Z",
    stop="2019-12-01T10:17:30Z",
    tags=["shock"],
    confidence=0.7,
    attributes={"spacecraft": "MMS1", "theta_bn": 72.0},
)
candidates.add_events([event])
```

Creating an event does not put it in a catalogue. `add_events` does that. The same event can
then go into other catalogues too.

Edit an event by setting its fields:

```python
event.status = "confirmed"
event.confidence = 0.9
event.set_attributes(mach=3.2)
event.delete()      # to the trash; event.restore() brings it back
```

### Many events at once

When your code writes hundreds of events, group them in one transaction. It is faster, and it
shows in the history as one change:

```python
with project.transaction():
    events = [
        project.create_event(start=s, stop=e, tags=["shock"], confidence=c)
        for s, e, c in detections
    ]
    candidates.add_events(events)
```

### Make sure it reached the server

Changes leave in the background, as you make them. Before your script ends, wait for the
server to confirm it has them:

```python
project.save()
```

## Save a version

When you reach a state worth keeping, name it:

```python
project.commit(name="detector v2 run", message="threshold 0.6, Dec 2019")
```

It shows in **History** in the browser with that name. Later you can compare with it, restore
it, or make a copy of it. See [Catalogues for a paper](paper.md).

```python
project.history()                  # every version, newest first
project.restore(version_id)        # the project as it was then
project.copy_version(version_id, "trial")   # that version, as a new project of yours
```

## Search

The same search language as the browser (see [Search](../reference/search.md)):

```python
strong = project.search("tag:shock confidence:>=0.8 NOT status:rejected")

project.create_smart_catalogue("Strong shocks", "tag:shock confidence:>=0.8")
```

## Import and export files

```python
from cocat_rs import tscat

project.import_votable("amda_catalogue.xml")
project.import_tfcat("type_iii.json")
tscat.import_json(project, "export_from_tscat.json")
tscat.import_database(project)      # your local tscat database (pip install 'cocat-rs[tscat]')

cc.export_votable([shocks], "shocks.xml")
```

More in [Bring your catalogues in](import-export.md).

## Share from Python

```python
info = cc.create_project("Bow shocks MMS")
cc.invite(info.id, "editor", email="colleague@example.org")
cc.members(info.id)
```

## Close when done

```python
project.close()
```

In a notebook you can just leave it open: it keeps in step with the others while the kernel
runs.

## Old cocat scripts

Scripts written for the older Python `cocat` package can run with a one-line change:

```python
from cocat_rs import api as cocat   # was: import cocat

cocat.set_config(host="https://cocat.my-lab.org", project_id="my-project")
cocat.log_in("you@example.org", "your password")
catalogue = cocat.get_catalogue("Bow shock crossings")
cocat.save()
```

`log_in`, `create_catalogue`, `create_event`, `get_catalogue`, `refresh`, `save`,
`import_votable` and `export_votable` keep their old names and arguments.

## When something goes wrong

| You see | It means | Do |
|---|---|---|
| `NotSignedIn` | no sign-in for this server, or it was revoked | run `cc.login()` again |
| `Forbidden: You cannot open …` | the project doesn't exist or you're not in it | check `cc.projects()` |
| `RuntimeError: this project is read-only for you (viewer)` | you are a viewer | ask the owner for the editor role |
| warning `Could not reach …: working on the local copy` | the server is out of reach | keep working; it syncs when the server answers |
| `ValueError` from `search` | the search has a typo | the message says where, and suggests a fix |

`project.status` tells you the state of the connection: `offline`, `connecting`, `synced`,
`unauthorized` (sign in again) or `no_access` (you were removed from the project).

See also the [Python cheat sheet](../reference/python.md).
