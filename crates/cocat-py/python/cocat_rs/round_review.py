"""The rules of reviewing a round, the same ones the web page applies.

A row stands for one candidate or proposed event: a dict with `event` (a dict with `uuid`,
`start`, `stop`, `attributes`), `proposal` (the proposer's email, `None` for a candidate),
`accepts`, `rejects`, `unsure`, `mine` (your verdict in the current revision), `previous`
(your last verdict from an earlier revision when you gave none since; it does not count as
reviewed) and `set_aside`.
"""

from . import _native
from ._native import final_catalogue_uuid

__all__ = [
    "final_catalogue_uuid",
    "final_events",
    "majority",
    "next_unreviewed",
    "progress",
    "reviewed",
    "rows",
]


def event_row(event):
    """An event, or a dict that already has `uuid`, `start`, `stop`, `attributes`, as a dict."""
    if isinstance(event, dict):
        return event
    return {
        "uuid": event.uuid,
        "start": event.start,
        "stop": event.stop,
        "attributes": event.attributes,
        "author": event.author,
        "created": event.created,
        "origin": event.origin,
        "status": event.status,
        "confidence": event.confidence,
        "start_err": event.start_err,
        "stop_err": event.stop_err,
        "shape": None if event.shape is None else [
            {"time": time, "value": value} for time, value in event.shape
        ],
        "source_id": event.source_id,
        "tags": sorted(event.tags),
        "products": sorted(event.products),
    }


def rows(proposals, verdicts, me):
    """One row per event of `proposals`, a list of `(author email or None for the
    candidates, events)`, with what `Client.verdicts` returned and your email `me`. Verdicts
    on events that are no longer there come last, with `set_aside` true."""
    listed = [(author, [event_row(event) for event in events]) for author, events in proposals]
    return _native.review_rows(listed, verdicts, me)


def progress(rows):
    """`(done, total, set_aside)`: the current events you reviewed, all current events, and
    the verdicts on events that are no longer current."""
    return _native.review_progress(rows)


def reviewed(rows):
    """How many current events any reviewer gave a verdict on, as far as you may read."""
    return _native.review_reviewed(rows)


def next_unreviewed(rows, start=0):
    """The index of the first row from `start` on, wrapping round, you have not reviewed."""
    return _native.review_next_unreviewed(rows, start)


def majority(rows):
    """For each row, whether more reviewers accepted than rejected; a tie keeps nothing."""
    return _native.review_majority(rows)


def final_events(rows, picked):
    """The events of the final catalogue, as dicts: copies of the rows `picked` (a list of
    booleans) holds, under fresh uuids, with `candidate`, `proposed_by`, `accepts`, `rejects`
    and `unsure` attributes."""
    return _native.review_final_events(rows, picked)
