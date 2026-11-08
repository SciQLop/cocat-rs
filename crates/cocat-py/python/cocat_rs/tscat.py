"""Bringing a tscat database or tscat's JSON files into a project, and writing that JSON back.

Uuids are kept, so importing again updates what an earlier import wrote. A tscat rating
(1 to 10) becomes a confidence (0 to 1), a removed item goes to the trash, and a dynamic
catalogue becomes a smart catalogue whose rule is its predicate. The conversion itself is
cocat-core's (`tscat_json.rs`), which the web app shares.
"""

import datetime
import json
from pathlib import Path

from ._native import export_tscat_json


def import_database(project):
    """Imports the local tscat database, read through tscat so it runs its own migrations.
    Returns the imported catalogues."""
    tscat = _tscat()
    events = [
        (event, removed)
        for removed in (False, True)
        for event in tscat.get_events(removed_items=removed)
    ]
    catalogues = [
        (catalogue, removed)
        for removed in (False, True)
        for catalogue in tscat.get_catalogues(removed_items=removed)
    ]
    predicates = _stored_predicates(tscat)
    data = {
        "events": [_event_json(event) for event, _ in events],
        "catalogues": [
            _catalogue_json(tscat, catalogue, predicates.get(catalogue.uuid))
            for catalogue, _ in catalogues
        ],
        "removed": [item.uuid for item, removed in events + catalogues if removed],
        "members": {
            catalogue.uuid: [event.uuid for event in tscat.get_events(catalogue)[0]]
            for catalogue, _ in catalogues
        },
    }
    return _import(project, json.dumps(data, default=_isoformat))


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


def import_json(project, path):
    """Imports a file tscat's `export_json` wrote. Returns the imported catalogues."""
    return _import(project, Path(path).read_text())


def export_json(project, catalogues, path):
    """Writes catalogues (objects, uuids or names) in tscat's JSON format. A smart
    catalogue is written with its current members as events and no predicate; derived
    intervals are left out, having no uuid."""
    catalogues = [_catalogue(project, catalogue) for catalogue in _listify(catalogues)]
    Path(path).write_text(export_tscat_json(catalogues))


def _import(project, text):
    return [project.catalogue(uuid) for uuid in project.import_tscat_json(text)]


def _tscat():
    try:
        import tscat
    except ImportError as error:
        raise ImportError(
            "importing a tscat database needs tscat: pip install 'cocat-rs[tscat]'"
        ) from error
    return tscat


def _event_json(event):
    return {**event.variable_attributes(), **event.fixed_attributes()}


def _catalogue_json(tscat, catalogue, predicate):
    listed = [
        event.uuid
        for gone in (False, True)
        for event in tscat.get_events(catalogue, removed_items=gone, assigned_only=True)[0]
    ]
    return {
        **catalogue.variable_attributes(),
        **{key: getattr(catalogue, key) for key in ("name", "author", "uuid", "tags")},
        "predicate": predicate,
        "events": listed,
    }


def _isoformat(value):
    """tscat's datetimes, as tscat's own JSON export writes them."""
    if isinstance(value, datetime.datetime):
        return value.isoformat()
    raise TypeError(f"{value!r} is not JSON")


def _catalogue(project, catalogue):
    return project.catalogue(catalogue) if isinstance(catalogue, str) else catalogue


def _listify(items):
    return items if isinstance(items, (list, tuple)) else [items]
