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
| `project.status` | `offline`, `connecting`, `synced`, `unauthorized`, `no_access` |
| `project.role`, `project.read_only` | your role in it |
| `with project.transaction(): ...` | group many changes into one |

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
| `catalogue.event_arrays()` | `uuid`, `start`, `stop` as arrays, fast for big catalogues |

## Events

| Call | Does |
|---|---|
| `project.create_event(start=..., stop=..., tags=..., attributes=..., confidence=..., status=...)` | a new event (not yet in a catalogue) |
| `project.event(uuid)` | one event |
| `event.start`, `event.stop`, `event.tags`, `event.status`, `event.confidence`, `event.comment` | read or set |
| `event.attributes`, `event.set_attributes(key=value)`, `event.remove_attributes("key")` | your own values |
| `event.catalogues` | uuids of the catalogues listing it |
| `event.delete()`, `event.restore()`, `event.purge()` | trash, back, gone for good |
| `project.trash()` | what is in the trash |
| `project.set_attribute_info("key", unit=..., description=...)` | document an attribute |

## Search

| Call | Does |
|---|---|
| `project.search("tag:shock confidence:>=0.8")` | matching events, in time order |
| `project.create_smart_catalogue("name", "query")` | a catalogue that updates itself |
| `catalogue.query` | a smart catalogue's search |
| `catalogue.materialize("name")` | a frozen copy of a smart catalogue |

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
| `cc.Conflict` | the server refused it in the current state, for example copying an empty public catalogue |
| `RuntimeError` "read-only" | you are a viewer here |
