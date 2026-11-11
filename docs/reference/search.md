# Search

The box above the events searches as you type. The same search works in Python with
`project.search("...")`.

## Plain words

Type a word: cocat finds the events whose tags, status, author or attributes contain it, or
whose start time begins with it.

- `shock` – anything mentioning shock
- `2015-03` – events starting in March 2015
- `"bow shock"` – the exact phrase

## Fields

Name a field to be precise:

| Search | Finds |
|---|---|
| `tag:shock` | events tagged `shock` |
| `status:reviewed` | events with that status |
| `confidence:>=0.8` | confidence 0.8 or more (also `>`, `<`, `<=`) |
| `confidence:[0.2 TO 0.5]` | confidence between 0.2 and 0.5 |
| `author:alice` | author contains "alice" |
| `author:=alice@lab.fr` | that exact author |
| `start:2015` | starts during 2015 |
| `start:2015-03` | starts during March 2015 |
| `start:[2015-01-01 TO 2016-06-30]` | starts between two dates; `*` leaves one end open |
| `created:>now-7d` | added in the last 7 days (`s`, `m`, `h`, `d`, `M` months, `y`) |
| `duration:>10min` | lasts more than 10 minutes (`s`, `min`, `h`, `d`) |
| `duration:[1h TO 2d]` | lasts between one hour and two days |
| `#density:>10` | an attribute, by its name after `#` |
| `#spacecraft:MMS1` | an attribute with that text |
| `catalogue:"Bow shocks"` | listed in that catalogue |
| `NOT _exists_:catalogue` | listed in no catalogue |
| `_exists_:shape` | has a time–frequency shape |

## Combining

- A space between two terms means **both**: `tag:shock confidence:>=0.8`
- `OR` means either: `tag:shock OR tag:discontinuity`
- `NOT` excludes: `tag:shock NOT status:rejected`
- Parentheses group: `(tag:shock OR tag:discontinuity) start:2019`

## Help while typing

The box suggests as you type: field names first, then the tags, statuses, authors, catalogues
and attributes already in the project. Arrow keys pick a suggestion, Enter or Tab takes it.

Each term shows as a coloured token. A field cocat doesn't know shows in red. A typo gives a
plain message: `there is no field "auhtor": did you mean "author"?`.

Not sure how to write it? **Build a search**, at the right of the box, writes it for you from
rows of field, test and value.

## Filter or search the whole project

On a catalogue, **Search in** under the box says what the search does:

- **This catalogue** filters what the catalogue holds. Nothing is saved.
- **All events** searches the whole project, and becomes the catalogue's own search: besides
  the events picked into it, the catalogue then holds every event the search finds. Press
  **Save** to keep it.

To search the whole project without touching any catalogue, open **All events** at the top of
the catalogue list.

## Smart catalogues

Any search can become a catalogue that updates itself. Press **Save as new smart
catalogue…** and give it a name.

The **+** next to Catalogues also offers ready-made ones: recently added, mine, not reviewed
yet, low confidence, with a shape.

A search with `now` in it is checked again whenever the project changes, not on a timer.

From Python:

```python
project.search("tag:shock confidence:>=0.8")            # the events, in time order
project.create_smart_catalogue("Strong shocks", "tag:shock confidence:>=0.8")
```

### Rules a search cannot say

Some selections are about time, not fields: "merge events closer than 10 minutes", "shocks
that happen within an hour after a CME", "intervals in A but not in B". These need a **rule**.
Open a catalogue and use **More → Add a rule…**. The editor starts with an example to change,
and tells you in words what the rule picks before you save it.

Rules that make new intervals (merging, shifting) give read-only intervals. **More → Freeze…**
turns them into normal events you can edit.
