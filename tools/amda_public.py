"""Imports AMDA's shared catalogues and time tables into a cocat server as public catalogues.

Each shared list becomes the public catalogue `public.amda.<AMDA id>`, filed under "AMDA/"
plus AMDA's folder, holding one catalogue with the list's intervals. Event uuids derive from
the AMDA id and the interval, so a second run updates in place: new intervals are added,
changed columns rewritten, and intervals AMDA dropped go to the trash.

    COCAT_PASSWORD=... python tools/amda_public.py --url URL --email EMAIL [--only ID ...]
        [--limit N] [--dry-run]

Needs speasy and cocat_rs, and a curator's account.
"""

from __future__ import annotations

import argparse
import math
import os
import re
import sys
import tempfile
import time
import uuid
from collections import Counter
from collections.abc import Iterable, Iterator
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any

import cocat_rs as cc

# Fixed forever: changing it changes every uuid and duplicates every event on the next run.
NAMESPACE = uuid.UUID("5b0f6a52-3c7e-4d61-9a0e-a3da00000001")
SOURCE = "AMDA"
MAX_DESCRIPTION = 2000
BATCH = 2000
SYNC_TIMEOUT = 600.0
ROOTS = (("Catalogs", "SharedCatalogs", "catalog"), ("TimeTables", "SharedTimeTables", "timetable"))
DATE = re.compile(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}")


@dataclass(frozen=True)
class SharedList:
    amda_id: str
    name: str
    folder: str
    description: str
    kind: str
    intervals: int

    @property
    def key(self) -> str:
        return f"amda.{self.amda_id}"

    @property
    def public_id(self) -> str:
        return f"public.{self.key}"

    @property
    def index_description(self) -> str:
        if len(self.description) <= MAX_DESCRIPTION:
            return self.description
        return self.description[: MAX_DESCRIPTION - 1] + "…"


@dataclass(frozen=True)
class Row:
    uuid: str
    start: datetime
    stop: datetime
    attributes: dict[str, Any]


@dataclass(frozen=True)
class Change:
    uuid: str
    set: dict[str, Any]
    removed: list[str]


@dataclass
class Plan:
    create: list[Row] = field(default_factory=list)
    restore: list[str] = field(default_factory=list)
    change: list[Change] = field(default_factory=list)
    trash: list[str] = field(default_factory=list)

    @property
    def empty(self) -> bool:
        return not (self.create or self.restore or self.change or self.trash)


def shared_lists(tree: Any) -> list[SharedList]:
    """Every shared catalogue and time table in speasy's AMDA tree, with its folder."""
    return [
        shared(leaf, path, kind)
        for top, root, kind in ROOTS
        for path, leaf in leaves(getattr(getattr(tree, top), root), [])
    ]


def leaves(node: Any, path: list[str]) -> Iterator[tuple[list[str], Any]]:
    for child in vars(node).values():
        if hasattr(child, "nbIntervals"):
            yield path, child
        elif hasattr(child, "spz_name"):
            yield from leaves(child, path + [child.spz_name()])


def shared(leaf: Any, path: list[str], kind: str) -> SharedList:
    return SharedList(
        amda_id=leaf.spz_uid(),
        name=leaf.spz_name(),
        folder="/".join([SOURCE, *path]),
        description=str(getattr(leaf, "description", "") or "").strip(),
        kind=kind,
        intervals=int(leaf.nbIntervals),
    )


def utc(moment: datetime) -> datetime:
    if moment.tzinfo is None:
        return moment.replace(tzinfo=timezone.utc)
    return moment.astimezone(timezone.utc)


def event_uuid(amda_id: str, start: datetime, stop: datetime, occurrence: int) -> str:
    name = f"{amda_id}/{utc(start).isoformat()}/{utc(stop).isoformat()}/{occurrence}"
    return str(uuid.uuid5(NAMESPACE, name))


def catalogue_uuid(amda_id: str) -> str:
    return str(uuid.uuid5(NAMESPACE, amda_id))


def rows(amda_id: str, intervals: Iterable[tuple[datetime, datetime, dict]]) -> list[Row]:
    """The list's intervals as events; the n-th copy of a (start, stop) pair gets occurrence n."""
    seen: Counter = Counter()
    made = []
    for start, stop, meta in intervals:
        start, stop = utc(start), utc(stop)
        occurrence = seen[(start, stop)]
        seen[(start, stop)] += 1
        made.append(
            Row(event_uuid(amda_id, start, stop, occurrence), start, stop, attributes(meta))
        )
    return made


def attributes(meta: dict) -> dict[str, Any]:
    """A row's columns as event attributes; empty values are left out."""
    values = {key: value_of(value) for key, value in meta.items()}
    return {key: value for key, value in values.items() if value is not None}


def value_of(value: Any) -> Any:
    if hasattr(value, "tolist"):
        value = value.tolist()
    if isinstance(value, bytes):
        value = value.decode("utf-8", "replace")
    if isinstance(value, (list, tuple)):
        return [value_of(item) for item in value]
    if isinstance(value, float) and not math.isfinite(value):
        return None
    if isinstance(value, datetime):
        return utc(value)
    if isinstance(value, str):
        return text_of(value.strip())
    return value


def text_of(text: str) -> Any:
    if not text:
        return None
    if DATE.match(text):
        try:
            return utc(datetime.fromisoformat(text))
        except ValueError:
            return text
    return text


def columns(intervals: Iterable[tuple[Any, Any, dict]]) -> list[str]:
    """The column names in AMDA's order, leaving out the ones no row fills."""
    names: dict[str, None] = {}
    for _, _, meta in intervals:
        for key, value in meta.items():
            if value_of(value) is not None:
                names.setdefault(key)
    return list(names)


def diff(wanted: list[Row], live: dict[str, dict], trashed: dict[str, dict]) -> Plan:
    """What turns the events the catalogue holds (`live`, and `trashed` in the trash) into
    `wanted`."""
    plan = Plan()
    for row in wanted:
        if row.uuid in live:
            current = live[row.uuid]
        elif row.uuid in trashed:
            current = trashed[row.uuid]
            plan.restore.append(row.uuid)
        else:
            plan.create.append(row)
            continue
        change = changed(row, current)
        if change.set or change.removed:
            plan.change.append(change)
    kept = {row.uuid for row in wanted}
    plan.trash = [uid for uid in live if uid not in kept]
    return plan


def changed(row: Row, current: dict) -> Change:
    return Change(
        row.uuid,
        {key: value for key, value in row.attributes.items() if current.get(key) != value},
        [key for key in current if key not in row.attributes],
    )


def batches(items: list, size: int) -> Iterator[list]:
    for start in range(0, len(items), size):
        yield items[start : start + size]


def fetch(shared_list: SharedList) -> list[tuple[datetime, datetime, dict]]:
    """The list's intervals from AMDA: (start, stop, columns)."""
    import speasy as spz

    if shared_list.kind == "catalog":
        product = spz.amda.get_catalog(shared_list.amda_id)
        return [(e.start_time, e.stop_time, dict(e.meta)) for e in product]
    product = spz.amda.get_timetable(shared_list.amda_id)
    return [(r.start_time, r.stop_time, {}) for r in product]


def amda_lists() -> list[SharedList]:
    import speasy as spz

    return shared_lists(spz.inventories.data_tree.amda)


def ensure_entry(url: str, cookie: str, shared_list: SharedList, known: set[str]) -> str:
    """Creates the public catalogue, or brings its entry up to date; says which."""
    fields = {
        "folder": shared_list.folder,
        "source": SOURCE,
        "description": shared_list.index_description,
    }
    if shared_list.public_id in known:
        cc.update_public_catalogue(
            url, cookie, shared_list.public_id, name=shared_list.name, **fields
        )
        return "updated"
    cc.create_public_catalogue(url, cookie, shared_list.name, key=shared_list.key, **fields)
    return "created"


def the_catalogue(project: Any, shared_list: SharedList, names: list[str]) -> Any:
    """The project's one catalogue, made or brought back if needed; any other goes to the trash."""
    wanted = catalogue_uuid(shared_list.amda_id)
    for other in project.catalogues():
        if other.uuid != wanted:
            other.delete()
    try:
        catalogue = project.catalogue(wanted)
        if catalogue.deleted:
            catalogue.restore()
    except cc.NotFound:
        catalogue = project.create_catalogue(name=shared_list.name, uuid=wanted, author=SOURCE)
    if catalogue.name != shared_list.name:
        catalogue.name = shared_list.name
    if catalogue.columns != names:
        catalogue.columns = names
    described = {
        "description": shared_list.description,
        "amda_id": shared_list.amda_id,
        "source": SOURCE,
    }
    if any(catalogue.attributes.get(key) != value for key, value in described.items()):
        catalogue.set_attributes(**described)
    return catalogue


def current(project: Any, catalogue: Any) -> tuple[dict[str, dict], dict[str, dict]]:
    arrays = catalogue.event_arrays(attributes=True)
    live = {uid: attrs for uid, attrs in zip(arrays["uuid"], arrays["attributes"]) if uid}
    trashed = {event.uuid: event.attributes for event in project.trash()}
    return live, trashed


def apply(project: Any, catalogue: Any, plan: Plan) -> None:
    for chunk in batches(plan.create, BATCH):
        with batch(project):
            for row in chunk:
                project.create_event(
                    row.start, row.stop, uuid=row.uuid, author=SOURCE, attributes=row.attributes
                )
            catalogue.add_events([row.uuid for row in chunk])
    for chunk in batches(plan.restore, BATCH):
        with batch(project):
            for uid in chunk:
                project.event(uid).restore()
    for chunk in batches(plan.change, BATCH):
        with batch(project):
            for change in chunk:
                event = project.event(change.uuid)
                if change.set:
                    event.set_attributes(**change.set)
                if change.removed:
                    event.remove_attributes(change.removed)
    for chunk in batches(plan.trash, BATCH):
        with batch(project):
            for uid in chunk:
                project.event(uid).delete()


class batch:
    """One merged update for the edits made inside."""

    def __init__(self, project: Any) -> None:
        self.project = project

    def __enter__(self) -> None:
        self.project.begin_batch()

    def __exit__(self, *_: object) -> None:
        self.project.end_batch()


def sync(url: str, cookie: str, shared_list: SharedList, intervals: list, cache: str) -> Plan:
    project = cc.Project.open(cache, shared_list.public_id)
    project.connect(url, cookie)
    try:
        project.wait_synced(SYNC_TIMEOUT)
        catalogue = the_catalogue(project, shared_list, columns(intervals))
        plan = diff(rows(shared_list.amda_id, intervals), *current(project, catalogue))
        apply(project, catalogue, plan)
        project.save(SYNC_TIMEOUT)
        return plan
    finally:
        project.close()


@dataclass
class Outcome:
    shared_list: SharedList
    entry: str
    plan: Plan
    events: int
    seconds: float

    def line(self) -> str:
        p = self.plan
        return (
            f"{self.shared_list.public_id}: {self.entry}, {self.events} events "
            f"(+{len(p.create)} new, {len(p.change)} changed, {len(p.restore)} restored, "
            f"{len(p.trash)} trashed) in {self.seconds:.1f} s"
        )


def import_one(
    url: str, cookie: str, shared_list: SharedList, known: set[str], dry_run: bool
) -> Outcome:
    started = time.monotonic()
    intervals = fetch(shared_list)
    if dry_run:
        entry = "would update" if shared_list.public_id in known else "would create"
        plan = Plan(create=rows(shared_list.amda_id, intervals))
        return Outcome(shared_list, entry, plan, len(intervals), time.monotonic() - started)
    entry = ensure_entry(url, cookie, shared_list, known)
    with tempfile.TemporaryDirectory(prefix="amda-public-") as cache:
        plan = sync(url, cookie, shared_list, intervals, cache)
    return Outcome(shared_list, entry, plan, len(intervals), time.monotonic() - started)


def chosen(lists: list[SharedList], only: list[str] | None, limit: int | None) -> list[SharedList]:
    if only:
        wanted = {name.removeprefix("amda.").removeprefix("amda/") for name in only}
        lists = [shared_list for shared_list in lists if shared_list.amda_id in wanted]
    return lists[:limit] if limit is not None else lists


def arguments(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Import AMDA's shared lists as public catalogues.")
    parser.add_argument("--url", required=True, help="the cocat server, https://cocat.example.org")
    parser.add_argument("--email", required=True, help="a curator's account")
    parser.add_argument(
        "--password-env",
        default="COCAT_PASSWORD",
        help="the environment variable holding the password (default COCAT_PASSWORD)",
    )
    parser.add_argument("--only", nargs="+", metavar="ID", help="AMDA ids, sharedcatalog_22 ...")
    parser.add_argument("--limit", type=int, help="import at most N lists")
    parser.add_argument("--dry-run", action="store_true", help="fetch and report, write nothing")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = arguments(argv)
    password = os.environ.get(args.password_env)
    if not password:
        print(f"Set the password in ${args.password_env}.", file=sys.stderr)
        return 2
    cookie = cc.login(args.url, args.email, password)
    known = {entry["id"] for entry in cc.public_catalogues(args.url, cookie)}
    lists = chosen(amda_lists(), args.only, args.limit)
    print(f"{len(lists)} shared lists, {sum(s.intervals for s in lists)} intervals")
    failed = []
    for shared_list in lists:
        try:
            print(import_one(args.url, cookie, shared_list, known, args.dry_run).line(), flush=True)
        except Exception as error:  # noqa: BLE001 - one broken list must not stop the others
            failed.append(shared_list.amda_id)
            print(f"{shared_list.public_id}: FAILED: {error}", flush=True)
    if failed:
        print(f"{len(failed)} failed: {' '.join(failed)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
