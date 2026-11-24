# Python cheat sheet

The most used calls of `cocat_rs`, on one page. The [Python guide](../guides/python.md)
explains them.

```python
import cocat_rs as cc
```

## Signing in

| Call | Does |
|---|---|
| `cc.login()` | sign in through the browser; remembered on this computer |
| `cc.login("https://server")` | the same, on another server |
| `cc.login(email, password)` | sign in without a browser, for this Python session only |
| `cc.logout()` | sign out and forget it |
| `cc.account()` | who you are: `id`, `email`, `is_curator` |

## Projects

| Call | Does |
|---|---|
| `cc.projects()` | your projects: `.id`, `.name`, `.role`, `.personal` |
| `cc.open()` | open My catalogues |
| `cc.open("name or id")` | open a project |
| `cc.create_project("name")` | a new project; returns its info |
| `project.save()` | wait until the server has every change |
| `project.close()` | stop syncing |
| `project.status` | `offline`, `connecting`, `synced`, `unauthorized`, `no_access`, `too_large` |
| `project.role`, `project.read_only` | your role in it |
| `with project.transaction(): ...` | group many changes into one; an error inside keeps none |

## Catalogues

| Call | Does |
|---|---|
| `project.catalogues()` | every catalogue |
| `project.catalogue("name or uuid")` | one catalogue |
| `project.create_catalogue(name=..., tags=..., attributes=...)` | a new catalogue |
| `catalogue.events` | its events (a set) |
| `catalogue.add_events([...])`, `catalogue.remove_events([...])` | change what it lists |
| `catalogue.name = "..."` | rename |
| `catalogue.delete()`, `catalogue.restore()` | to the trash and back |
| `catalogue.event_arrays()` | `uuid`, `start`, `stop` as arrays, fast for big catalogues; `attributes=True` adds `attributes`, `tags` and `confidence` |
| `catalogue.to_pandas()`, `catalogue.to_polars()` | the events as a DataFrame |

## Events

| Call | Does |
|---|---|
| `project.create_event(start=..., stop=..., tags=..., attributes=..., confidence=..., status=...)` | a new event (not yet in a catalogue) |
| `project.event(uuid)` | one event |
| `event.start`, `event.stop`, `event.tags`, `event.status`, `event.confidence`, `event.comment` | read or set |
| `event.range = (start, stop)` | move both ends at once |
| `event.attributes`, `event.set_attributes(key=value)`, `event.remove_attributes("key")` | your own values |
| `event.catalogues` | uuids of the catalogues listing it |
| `event.delete()`, `event.restore()`, `event.purge()` | trash, back, gone for good |
| `project.trash()` | what is in the trash |
| `project.set_attribute_info("key", unit=..., description=..., type=...)` | document an attribute; `type` only for a new one |

## Search

| Call | Does |
|---|---|
| `project.search("tag:shock confidence:>=0.8")` | matching events, in time order |
| `project.create_smart_catalogue("name", "query")` | a catalogue that updates itself |
| `catalogue.query` | a smart catalogue's search |
| `catalogue.set_query("query", source=other)` | search the events of another catalogue; `catalogue.query_source` reads it back |
| `catalogue.rule = None` | remove the search: the catalogue keeps the events it lists |
| `catalogue.materialize("name")` | a frozen copy of a smart catalogue |
| `project.parse_query("query", source=None)` | the rule of a search, catalogue names read in this project |
| `project.preview("query", limit=50)` | what a search would hold, nothing saved: arrays of the first 50, and `count` |

### Building a search from rows

What a search editor needs, in the same words as the browser's builder:

| Call | Gives |
|---|---|
| `cc.query_fields()` | each field's `name`, `kind` and `label` |
| `cc.query_tests("time")` | the tests a kind offers: `test`, `label`, `needs_value` |
| `cc.clause("confidence", "at_least", "0.8")` | `"confidence:>=0.8"` |
| `cc.search_text(clauses, any=True)` | the rows joined with OR (all of them: with spaces) |
| `project.builder_rows(rule)` | `(any, rows)` back from a rule, or `None` when the search is too rich for rows |

### A search box over an event table

What the browser's filter box does, for an app that shows events in a table:

| Call | Gives |
|---|---|
| `project.query_kind(text)` | `"empty"`, `"words"`, `"rule"` or `"broken"` |
| `project.matches("tag:shock", uuids, source=None)` | a numpy bool mask aligned to `uuids`: what a smart catalogue with that search would hold; `(start, stop)` for an interval with no uuid |
| `project.filter_words("shock alice", uuids)` | the plain-words filter's mask |
| `cc.pieces(text)` | the text in parts to colour; joined, they give the text back |
| `cc.complete(text, cursor, project.vocabulary())` | suggestions for the word at the cursor |
| `cc.accept(text, completion, suggestion)` | the new text and cursor |
| `cc.presets("me@lab.fr")` | the preset searches, as `name` and `query` |

## History

| Call | Does |
|---|---|
| `project.commit(name=..., message=...)` | save a named version |
| `project.history()` | every version, newest first |
| `project.history(catalogue_uuid)` | versions that changed that catalogue |
| `project.restore(version)` | the project as it was then |
| `project.restore(version, catalogue_uuid)` | only that catalogue |
| `project.copy_version(version, "name")` | that version, as a new project of yours |
| `project.name_version(version, "name")` | name an earlier version |

## Files

| Call | Does |
|---|---|
| `project.import_votable(path)` | VOTable in |
| `project.import_tfcat(path)` | TFCat in |
| `cc.export_votable([catalogue, ...], path)` | VOTable out |
| `cc.export_tfcat(catalogue, path)` | TFCat out |
| `project.import_hpevent(path, name=None)` | HPEvent list in |
| `cc.export_hpevent([catalogue], path)` | HPEvent list out |
| `project.import_csv(path, name=None)` | CSV in |
| `cc.export_csv([catalogue, ...], path)` | CSV out |
| `catalogue.to_pandas()`, `catalogue.to_polars()` | a DataFrame, one row per event |
| `project.import_dataframe(df, name)` | a DataFrame in, as a new catalogue |
| `cocat_rs.tscat.import_database(project)` | your local tscat database in |
| `cocat_rs.tscat.import_json(project, path)` / `export_json(project, catalogues, path)` | tscat JSON |

## Sharing

| Call | Does |
|---|---|
| `cc.invite(project_id, "editor", email="...")` | invite someone (`viewer` or `editor`) |
| `cc.members(project_id)` | who is in the project |
| `cc.set_role(project_id, user_id, "viewer")` | change a role |
| `cc.remove_member(project_id, user_id)` | remove someone |
| `cc.project_url(project_id)` | the project's web address |

## Public catalogues

| Call | Does |
|---|---|
| `cc.public_catalogues()` | the list, with `id`, `name`, `folder`, `count` |
| `cc.copy_public_catalogue(public_id, project_id=None)` | a copy to work on |
| `project.submit(catalogue, title=..., description=...)` | offer yours |
| `cc.propose_change(public_id)` then `project.submit_update(copy)` | propose a fix |
| `cc.submissions()` | what you submitted, and its state |

## Rounds

| Call | Does |
|---|---|
| `project.rounds()` | the project's rounds |
| `project.create_round(name, kind="review", candidates=catalogue, visibility="blind")` | start one |
| `project.verdict(round_id, event, "accept")` | `accept`, `reject`, `unsure` or `None` |
| `project.set_verdicts(round_id, {uuid: verdict})` | many at once |
| `project.verdicts(round_id)` | the counts, as far as you may see them |

## Errors

| Error | Means |
|---|---|
| `cc.NotSignedIn` | run `cc.login()` |
| `cc.Forbidden` | not allowed: not a member, or not the right role |
| `cc.NotFound` | no such catalogue, event or project |
| `cc.QueryError` | a search that does not read; `position`, `length`, `message` and `suggestion` say where and what was meant |
| `cc.Conflict` | the server refused it in the current state, for example copying an empty public catalogue |
| `RuntimeError` "read-only" | you are a viewer here |
