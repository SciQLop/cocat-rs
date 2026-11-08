"""The pure parts of the AMDA importer, against small stand-ins for speasy's objects."""

import math
import uuid
from datetime import datetime, timezone

import amda_public as ap
import numpy as np

UTC = timezone.utc


class Node:
    """A folder or a list of speasy's inventory tree: children are attributes."""

    def __init__(self, name, **children):
        self._name = name
        self.__dict__.update(children)

    def spz_name(self):
        return self._name


class Leaf(Node):
    def __init__(self, uid, name, description="", nbIntervals="3"):
        super().__init__(name)
        self._uid = uid
        self.description = description
        self.nbIntervals = nbIntervals

    def spz_uid(self):
        return self._uid


def tree():
    return Node(
        "amda",
        Catalogs=Node(
            "Catalogs",
            SharedCatalogs=Node(
                "SharedCatalogs",
                EARTH=Node(
                    "EARTH",
                    cat22=Leaf("sharedcatalog_22", "model_regions", " Breuillard et al. "),
                ),
            ),
        ),
        TimeTables=Node(
            "TimeTables",
            SharedTimeTables=Node(
                "SharedTimeTables",
                Solar=Node(
                    "Solar wind",
                    ICME=Node("ICME", tt3=Leaf("sharedtimeTable_3", "ICME_list")),
                ),
                tt0=Leaf("sharedtimeTable_0", "loose"),
            ),
        ),
    )


def at(text):
    return datetime.fromisoformat(text).replace(tzinfo=UTC)


def test_the_shared_lists_carry_their_amda_folder_and_kind():
    lists = ap.shared_lists(tree())
    assert [(s.amda_id, s.kind, s.folder) for s in lists] == [
        ("sharedcatalog_22", "catalog", "AMDA/EARTH"),
        ("sharedtimeTable_3", "timetable", "AMDA/Solar wind/ICME"),
        ("sharedtimeTable_0", "timetable", "AMDA"),
    ]
    catalogue = lists[0]
    assert catalogue.name == "model_regions"
    assert catalogue.description == "Breuillard et al."
    assert catalogue.intervals == 3
    assert catalogue.key == "amda.sharedcatalog_22"
    assert catalogue.public_id == "public.amda.sharedcatalog_22"


def test_a_long_description_is_cut_for_the_index_only():
    shared = ap.SharedList("sharedcatalog_1", "x", "AMDA", "d" * 3000, "catalog", 1)
    assert len(shared.index_description) == ap.MAX_DESCRIPTION
    assert shared.index_description.endswith("…")
    assert ap.SharedList("i", "x", "AMDA", "short", "catalog", 1).index_description == "short"


def test_event_uuids_are_stable_and_tell_duplicates_apart():
    start, stop = at("2020-01-01T00:00:00"), at("2020-01-01T01:00:00")
    rows = ap.rows("sharedcatalog_22", [(start, stop, {}), (start, stop, {}), (stop, stop, {})])
    again = ap.rows("sharedcatalog_22", [(start, stop, {}), (start, stop, {}), (stop, stop, {})])
    assert [r.uuid for r in rows] == [r.uuid for r in again]
    assert len({r.uuid for r in rows}) == 3
    assert rows[0].uuid == ap.event_uuid("sharedcatalog_22", start, stop, 0)
    assert rows[1].uuid == ap.event_uuid("sharedcatalog_22", start, stop, 1)
    assert ap.event_uuid("sharedcatalog_23", start, stop, 0) != rows[0].uuid
    uuid.UUID(rows[0].uuid)


def test_naive_times_are_read_as_utc():
    naive = datetime(2020, 1, 1)
    assert ap.event_uuid("a", naive, naive, 0) == ap.event_uuid(
        "a", at("2020-01-01T00:00:00"), at("2020-01-01T00:00:00"), 0
    )
    [row] = ap.rows("a", [(naive, naive, {})])
    assert row.start == at("2020-01-01T00:00:00")


def test_columns_become_typed_attributes():
    meta = {
        "flag": np.bool_(True),
        "n": np.int32(7),
        "speed": np.float64(412.5),
        "missing": np.float64(math.nan),
        "none": None,
        "label": b" MSH ",
        "blank": "  ",
        "region": "magnetosheath",
        "seen": "2019-01-01T00:00:04.000",
        "vector": np.array([1.0, 2.0]),
    }
    assert ap.attributes(meta) == {
        "flag": True,
        "n": 7,
        "speed": 412.5,
        "label": "MSH",
        "region": "magnetosheath",
        "seen": at("2019-01-01T00:00:04"),
        "vector": [1.0, 2.0],
    }
    assert type(ap.attributes(meta)["n"]) is int


def test_the_columns_keep_amda_order_without_the_empty_ones():
    events = [(None, None, {"b": 1, "a": None}), (None, None, {"b": 2, "a": "x", "c": None})]
    assert ap.columns(events) == ["b", "a"]


def row(uid, **attributes):
    return ap.Row(uid, at("2020-01-01T00:00:00"), at("2020-01-01T01:00:00"), attributes)


def test_the_diff_creates_restores_changes_and_trashes():
    wanted = [row("new", q=1), row("same", q=1), row("changed", q=2, k="x"), row("back", q=3)]
    live = {"same": {"q": 1}, "changed": {"q": 1, "old": True}, "gone": {"q": 9}}
    trashed = {"back": {"q": 4}}

    plan = ap.diff(wanted, live, trashed)

    assert [r.uuid for r in plan.create] == ["new"]
    assert plan.restore == ["back"]
    assert plan.change == [
        ap.Change("changed", {"q": 2, "k": "x"}, ["old"]),
        ap.Change("back", {"q": 3}, []),
    ]
    assert plan.trash == ["gone"]
    assert not plan.empty


def test_a_second_run_over_the_same_rows_changes_nothing():
    wanted = [row("a", q=1.5, when=at("2019-01-01T00:00:00")), row("b")]
    live = {"a": {"q": 1.5, "when": at("2019-01-01T00:00:00")}, "b": {}}
    assert ap.diff(wanted, live, {}).empty


def test_batches_cut_a_list_in_order():
    assert list(ap.batches(list(range(5)), 2)) == [[0, 1], [2, 3], [4]]
    assert list(ap.batches([], 2)) == []
