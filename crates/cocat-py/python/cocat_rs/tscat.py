"""Bringing a tscat database or tscat's JSON files into a project, and writing that JSON back.

Uuids are kept, so importing again updates what an earlier import wrote. A tscat rating
(1 to 10) becomes a confidence (0 to 1), a removed item goes to the trash, and a dynamic
catalogue becomes a smart catalogue whose rule is its predicate.
"""

import datetime
import json
import re
import warnings
from pathlib import Path

from ._native import NotFound, rule_from_tscat
from ._old_names import renamed_keywords

UTC = datetime.timezone.utc
ORIGIN = "tscat"
EVENT_FIELDS = ("start", "stop", "author", "uuid", "tags", "products", "rating")
CATALOGUE_FIELDS = ("name", "author", "uuid", "tags", "predicate", "events")
# tscat stores a datetime attribute as orjson's text of it, so its type is lost; a value of
# exactly that form is taken back as a datetime, which the project registers as a date.
TSCAT_DATETIME = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,6})?(\+00:00)?")


@renamed_keywords(room="project")
def import_database(project):
    """Imports the local tscat database, read through tscat so it runs its own migrations.
    Returns the imported catalogues."""
    tscat = _tscat()
    events = [
        _event_of_tscat(event)
        for removed in (False, True)
        for event in tscat.get_events(removed_items=removed)
    ]
    predicates = _stored_predicates(tscat)
    catalogues = [
        _catalogue_of_tscat(tscat, catalogue, predicates.get(catalogue.uuid))
        for removed in (False, True)
        for catalogue in tscat.get_catalogues(removed_items=removed)
    ]
    return _write(project, events, catalogues)


def _stored_predicates(tscat):
    """Each catalogue's predicate as tscat stored it. tscat 0.5.1 rebuilds InCatalogue
    through a lookup that returns its first catalogue, whatever the uuid; the stored form
    still names the right one."""
    from sqlalchemy import select
    from tscat.orm_sqlalchemy import orm

    rows = tscat.base.backend().session.execute(
        select(orm.Catalogue.uuid, orm.Catalogue.predicate)
    )
    return {uuid: predicate for uuid, predicate in rows if predicate}


@renamed_keywords(room="project")
def import_json(project, path):
    """Imports a file tscat's `export_json` wrote. Returns the imported catalogues."""
    data = json.loads(Path(path).read_text())
    events = [_event_of_json(event) for event in data["events"]]
    catalogues = [_catalogue_of_json(catalogue) for catalogue in data["catalogues"]]
    return _write(project, events, catalogues)


@renamed_keywords(room="project")
def export_json(project, catalogues, path):
    """Writes catalogues (objects, uuids or names) in tscat's JSON format. A smart
    catalogue is written with its current members as events and no predicate; derived
    intervals are left out, having no uuid."""
    catalogues = [_catalogue(project, catalogue) for catalogue in _listify(catalogues)]
    events = {
        event.uuid: _event_to_json(event)
        for catalogue in catalogues
        for event in catalogue.events
    }
    data = {
        "catalogues": [_catalogue_to_json(catalogue) for catalogue in catalogues],
        "events": list(events.values()),
    }
    Path(path).write_text(json.dumps(data))


def _tscat():
    try:
        import tscat
    except ImportError as error:
        raise ImportError(
            "importing a tscat database needs tscat: pip install 'cocat-rs[tscat]'"
        ) from error
    return tscat


def _event_of_tscat(event):
    fields = event.fixed_attributes()
    return {**fields, "attributes": event.variable_attributes(), "removed": event.is_removed()}


def _catalogue_of_tscat(tscat, catalogue, predicate):
    listed = [
        event.uuid
        for gone in (False, True)
        for event in tscat.get_events(catalogue, removed_items=gone, assigned_only=True)[0]
    ]
    members = [event.uuid for event in tscat.get_events(catalogue)[0]]
    return {
        **{key: getattr(catalogue, key) for key in ("name", "author", "uuid", "tags")},
        "attributes": catalogue.variable_attributes(),
        "predicate": predicate,
        "events": listed,
        "members": members,
        "removed": catalogue.is_removed(),
    }


def _event_of_json(event):
    fields = {key: event.get(key) for key in EVENT_FIELDS}
    attributes = {key: value for key, value in event.items() if key not in EVENT_FIELDS}
    return {**fields, "attributes": attributes, "removed": False}


def _catalogue_of_json(catalogue):
    """tscat writes a predicate as its Python repr, which says nothing a rule can use: such
    a catalogue keeps the members tscat wrote, as listed events."""
    predicate = catalogue.get("predicate")
    if isinstance(predicate, str):
        warnings.warn(
            f"catalogue {catalogue['name']}: tscat wrote its predicate as text, "
            "so it is imported with its members and no rule"
        )
        predicate = None
    attributes = {
        key: value for key, value in catalogue.items() if key not in CATALOGUE_FIELDS
    }
    return {
        **{key: catalogue.get(key) for key in ("name", "author", "uuid", "tags")},
        "attributes": attributes,
        "predicate": predicate,
        "events": catalogue["events"],
        "members": catalogue["events"],
        "removed": False,
    }


def _write(project, events, catalogues):
    with project.transaction():
        _ensure_origin(project)
        for event in events:
            _put_event(project, event)
        return [_put_catalogue(project, catalogue) for catalogue in catalogues]


def _ensure_origin(project):
    try:
        project.origin(ORIGIN)
    except NotFound:
        project.create_origin("import", "tscat", id=ORIGIN)


def _put_event(project, event):
    fields = _event_fields(event)
    try:
        existing = project.event(event["uuid"])
    except NotFound:
        created = project.create_event(uuid=event["uuid"], origin=ORIGIN, **fields)
        _set_trash(created, event["removed"], in_trash=False)
        return
    existing.range = (fields["start"], fields["stop"])
    for key in ("author", "tags", "products", "confidence"):
        setattr(existing, key, fields[key])
    _replace_attributes(existing, fields["attributes"])
    _set_trash(existing, event["removed"], in_trash=existing.status == "deleted")


def _event_fields(event):
    rating = event["rating"]
    return {
        "start": _utc(event["start"]),
        "stop": _utc(event["stop"]),
        "author": event["author"] or None,
        "tags": list(event["tags"] or []),
        "products": list(event["products"] or []),
        "confidence": None if rating is None else rating / 10,
        "attributes": {key: _value(value) for key, value in event["attributes"].items()},
    }


def _put_catalogue(project, catalogue):
    rule, listed = _rule_and_listed(catalogue)
    attributes = {key: _value(value) for key, value in catalogue["attributes"].items()}
    try:
        existing = project.catalogue(catalogue["uuid"])
    except NotFound:
        existing = project.create_catalogue(
            name=catalogue["name"],
            author=catalogue["author"] or None,
            uuid=catalogue["uuid"],
            tags=list(catalogue["tags"] or []),
            attributes=attributes,
            events=listed,
        )
    else:
        existing.name = catalogue["name"]
        existing.tags = list(catalogue["tags"] or [])
        _replace_attributes(existing, attributes)
        existing.rule = None
        keep = set(listed)
        existing.remove_events([e for e in existing.events if e.uuid not in keep])
        existing.add_events(listed)
    existing.rule = rule
    _set_trash(existing, catalogue["removed"], in_trash=existing.deleted)
    return existing


def _rule_and_listed(catalogue):
    """The rule of a dynamic catalogue and the events it lists. A predicate that does not
    convert leaves the catalogue static, listing every member tscat gives it now."""
    predicate = catalogue["predicate"]
    if predicate is None:
        return None, catalogue["events"]
    try:
        return rule_from_tscat(predicate), catalogue["events"]
    except ValueError as error:
        warnings.warn(
            f"catalogue {catalogue['name']}: its predicate does not convert ({error}), "
            "so it is imported with its current members and no rule"
        )
        return None, catalogue["members"]


def _replace_attributes(target, attributes):
    stale = [key for key in target.attributes if key not in attributes]
    if stale:
        target.remove_attributes(stale)
    if attributes:
        target.set_attributes(**attributes)


def _set_trash(target, removed, in_trash):
    """Restores only what is in the trash, so a status set in the project stays."""
    if removed:
        target.delete()
    elif in_trash:
        target.restore()


def _utc(time):
    if isinstance(time, str):
        time = datetime.datetime.fromisoformat(time)
    return time if time.tzinfo is not None else time.replace(tzinfo=UTC)


def _value(value):
    if isinstance(value, str) and TSCAT_DATETIME.fullmatch(value):
        return _utc(value)
    if isinstance(value, datetime.datetime):
        return _utc(value)
    return value


def _catalogue(project, catalogue):
    return project.catalogue(catalogue) if isinstance(catalogue, str) else catalogue


def _listify(items):
    return items if isinstance(items, (list, tuple)) else [items]


def _event_to_json(event):
    return {
        "start": _tscat_time(event.start),
        "stop": _tscat_time(event.stop),
        "author": event.author or "",
        "uuid": event.uuid,
        "tags": sorted(event.tags),
        "products": sorted(event.products),
        "rating": _rating(event.confidence),
        **_attributes_to_json(event.attributes),
    }


def _catalogue_to_json(catalogue):
    return {
        "name": catalogue.name,
        "author": catalogue.author or "",
        "uuid": catalogue.uuid,
        "tags": sorted(catalogue.tags),
        "predicate": None,
        **_attributes_to_json(catalogue.attributes),
        "events": sorted(event.uuid for event in catalogue.events),
    }


def _attributes_to_json(attributes):
    return {
        key: value.isoformat() if isinstance(value, datetime.datetime) else value
        for key, value in attributes.items()
    }


def _tscat_time(time):
    """tscat writes naive UTC times."""
    return time.astimezone(UTC).replace(tzinfo=None).isoformat()


def _rating(confidence):
    """tscat takes a whole rating from 1 to 10."""
    if confidence is None:
        return None
    return min(10, max(1, round(confidence * 10)))
