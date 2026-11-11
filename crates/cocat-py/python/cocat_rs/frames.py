"""Catalogues as pandas or polars DataFrames, and DataFrames as catalogues.

A row is an event: `uuid`, `start`, `stop`, `tags`, `status`, `confidence`, `author`,
`comment`, then one column per attribute. Reading a frame back takes the same columns; any
other column is an attribute, and a row whose `uuid` the project holds updates that event.
Neither library is a dependency: each is imported when used.
"""

import re

FIELDS = ("uuid", "start", "stop", "tags", "status", "confidence", "author", "comment")
_SET_ON_UPDATE = ("status", "confidence", "author", "comment")


def rows(catalogue):
    """The catalogue's events as dicts, in time order, and the attribute columns, sorted."""
    events = sorted(catalogue.events, key=lambda event: (event.start, event.uuid))
    keys = sorted({key for event in events for key in event.attributes} - set(FIELDS))
    return [_row(event) for event in events], [*FIELDS, *keys]


def _row(event):
    fields = {name: getattr(event, name) for name in FIELDS}
    fields["tags"] = sorted(event.tags)
    extra = {key: value for key, value in event.attributes.items() if key not in fields}
    return {**fields, **extra}


def to_pandas(catalogue):
    import pandas as pd

    table, columns = rows(catalogue)
    return pd.DataFrame(table, columns=columns)


def to_polars(catalogue):
    import polars as pl

    table, columns = rows(catalogue)
    if not table:
        return pl.DataFrame(schema=columns)
    # Attributes are typed per event, not per column: let polars find a common type.
    return pl.DataFrame(table, schema=columns, strict=False, infer_schema_length=None)


def import_frame(project, frame, name):
    """A new catalogue named `name` holding a row's event each; returns it."""
    records = frame.to_dicts() if hasattr(frame, "to_dicts") else frame.to_dict("records")
    columns = list(frame.columns)
    for required in ("start", "stop"):
        if required not in columns:
            raise ValueError(f"The DataFrame has no {required} column: it needs a start and a stop.")
    with project.transaction():
        events = [_event(project, _present(record)) for record in records]
        return project.create_catalogue(name=name, events=events)


def _event(project, record):
    start, stop = record.pop("start"), record.pop("stop")
    uuid = record.pop("uuid", None)
    tags = _tags(record.pop("tags", None))
    fields = {key: record.pop(key) for key in _SET_ON_UPDATE if key in record}
    try:
        event = project.event(uuid) if uuid else None
    except LookupError:
        event = None
    if event is None:
        return project.create_event(
            start, stop, uuid=uuid, tags=tags, attributes=record, **fields
        )
    event.range = (start, stop)
    if tags is not None:
        event.tags = tags
    for key, value in fields.items():
        setattr(event, key, value)
    if record:
        event.set_attributes(**record)
    return event


def _present(record):
    """The row's set cells, as plain Python values."""
    return {key: _plain(value) for key, value in record.items() if not _missing(value)}


def _missing(value):
    if value is None:
        return True
    if hasattr(value, "__len__"):
        return False
    try:
        return bool(value != value)  # NaN and NaT differ from themselves
    except TypeError:
        return True  # pandas.NA refuses to be a bool


def _plain(value):
    if hasattr(value, "to_pydatetime"):
        return value.to_pydatetime()
    if hasattr(value, "tolist"):
        return value.tolist()  # numpy scalars and arrays
    return value


def _tags(value):
    if value is None:
        return None
    if isinstance(value, str):
        value = re.split(r"[;,]", value)
    return [tag.strip() for tag in value if str(tag).strip()]
