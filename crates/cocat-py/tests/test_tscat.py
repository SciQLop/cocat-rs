"""The tscat import and tscat's JSON format, checked against tscat itself."""

import datetime
import json

import pytest
import tscat
import tscat.base
from tscat import filtering as f
from tscat.import_export import export_json as tscat_export_json
from tscat.import_export import import_json_file as tscat_import_json_file

import cocat_rs as cc
from cocat_rs import tscat as cc_tscat

UTC = datetime.timezone.utc
DAY = datetime.datetime(2020, 1, 1)


def hours(n):
    return DAY + datetime.timedelta(hours=n)


@pytest.fixture
def database(tmp_path, monkeypatch):
    """A tscat database of its own: tscat keeps it under appdirs' user data directory,
    which XDG_DATA_HOME sets on Linux; tscat opens it on first use."""
    monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path / "tscat-data"))
    monkeypatch.setattr(tscat.base, "_backend", None)
    yield tscat
    if tscat.base._backend is not None:
        tscat.base._backend.close()


@pytest.fixture
def project(cache):
    return cc.Project.open(cache, "75ca7000-0000-4000-8000-000000000000")


@pytest.fixture
def filled(database):
    """Every field, a removed event, a removed catalogue and dynamic catalogues whose
    predicates use every node tscat has."""
    alice = tscat.create_event(
        hours(0), hours(1), author="alice", tags=["a", "b"], products=["speasy/x"],
        rating=7, n=3, x=1.5, flag=True, label="hi", when=datetime.datetime(2020, 1, 2),
    )
    bob = tscat.create_event(
        hours(2), hours(3), author="bob", rating=None, n=10, label="other",
        when=datetime.datetime(2020, 1, 5),
    )
    gone = tscat.create_event(hours(4), hours(4), author="carol", tags=["c"], rating=3)
    gone.remove()
    nameless = tscat.create_event(hours(5), hours(6), author="", rating=10)
    # tscat 0.5.1's get_catalogue(uuid=...) ignores the uuid and returns the first
    # catalogue, and reading an InCatalogue predicate back goes through it: the catalogue
    # InCatalogue names has to be the first one for tscat itself to be right.
    listed = tscat.create_catalogue("listed", author="alice", tags=["t"], k=1)
    tscat.add_events_to_catalogue(listed, [alice, gone, nameless])
    trashed = tscat.create_catalogue("trashed", author="bob")
    tscat.add_events_to_catalogue(trashed, [bob])
    trashed.remove()
    tscat.create_catalogue(
        "smart",
        author="alice",
        predicate=f.Any(
            f.All(f.Comparison(">", f.Field("rating"), 5), f.In("a", f.Field("tags"))),
            f.All(f.Comparison("==", f.Field("rating"), None), f.Has(f.Attribute("n"))),
            f.All(
                f.Not(f.Match(f.Field("author"), "^(alice|bob)$")),
                f.InCatalogue(listed),
            ),
        ),
    )
    tscat.create_catalogue(
        "late",
        author="bob",
        predicate=f.Comparison(">", f.Attribute("when"), datetime.datetime(2020, 1, 3)),
    )
    tscat.create_catalogue(
        "rated", author="bob", predicate=f.Comparison("!=", f.Field("rating"), None)
    )
    tscat.save()
    return database


def tscat_catalogue(**wanted):
    """tscat 0.5.1's get_catalogue ignores uuid and name, so look the catalogue up here."""
    (key, value), = wanted.items()
    return next(c for c in tscat_catalogues() if getattr(c, key) == value)


def tscat_events():
    return tscat.get_events() + tscat.get_events(removed_items=True)


def tscat_catalogues():
    return tscat.get_catalogues() + tscat.get_catalogues(removed_items=True)


def utc(time):
    return time.replace(tzinfo=UTC)


def expected_attribute(value):
    if isinstance(value, str) and value.endswith("+00:00"):
        return datetime.datetime.fromisoformat(value)
    return value


def assert_event_matches(project, event):
    ours = project.event(event.uuid)
    assert (ours.start, ours.stop) == (utc(event.start), utc(event.stop))
    assert ours.author == (event.author or None)
    assert ours.tags == set(event.tags)
    assert ours.products == set(event.products)
    assert ours.confidence == (None if event.rating is None else event.rating / 10)
    assert ours.status == ("deleted" if event.is_removed() else "confirmed")
    assert ours.attributes == {
        key: expected_attribute(value) for key, value in event.variable_attributes().items()
    }


def member_uuids(catalogue):
    return {event.uuid for event in catalogue.events}


def tscat_member_uuids(catalogue):
    return {event.uuid for event in tscat.get_events(catalogue)[0]}


def test_every_event_field_imports(filled, project):
    cc_tscat.import_database(project)
    events = tscat_events()
    assert len(events) == 4
    for event in events:
        assert_event_matches(project, event)
        assert project.event(event.uuid).origin == "tscat"
    assert project.origin("tscat")["kind"] == "import"
    assert project.attribute_types()["when"]["type"] == "date"
    assert len(project.trash()) == 1


def test_every_catalogue_imports_with_tscats_members(filled, project):
    imported = cc_tscat.import_database(project)
    catalogues = tscat_catalogues()
    assert {catalogue.uuid for catalogue in imported} == {c.uuid for c in catalogues}
    for catalogue in catalogues:
        ours = project.catalogue(catalogue.uuid)
        assert ours.name == catalogue.name
        assert ours.author == catalogue.author
        assert ours.tags == set(catalogue.tags)
        assert ours.attributes == catalogue.variable_attributes()
        assert ours.deleted == catalogue.is_removed()
        assert (ours.rule is None) == (catalogue.predicate is None)
        assert member_uuids(ours) == tscat_member_uuids(catalogue), catalogue.name


def test_the_smart_catalogues_match_tscats_filter(filled, project):
    cc_tscat.import_database(project)
    expected = {"smart": 3, "late": 1, "rated": 2}
    for name, count in expected.items():
        catalogue = tscat_catalogue(name=name)
        assert len(tscat_member_uuids(catalogue)) == count, name
        assert member_uuids(project.catalogue(catalogue.uuid)) == tscat_member_uuids(catalogue)


def test_importing_again_updates_instead_of_duplicating(filled, project):
    cc_tscat.import_database(project)
    listed = tscat_catalogue(name="listed")
    alice = next(e for e in tscat.get_events() if e.author == "alice")
    gone = tscat.get_events(removed_items=True)[0]
    alice.rating = 2
    alice.tags = ["z"]
    del alice.label
    gone.restore()
    listed.name = "renamed"
    tscat.remove_events_from_catalogue(listed, [alice])
    tscat.save()

    cc_tscat.import_database(project)

    assert len(project.catalogues()) == len(tscat.get_catalogues())
    for event in tscat_events():
        assert_event_matches(project, event)
    ours = project.catalogue(listed.uuid)
    assert ours.name == "renamed"
    assert member_uuids(ours) == tscat_member_uuids(listed)
    for catalogue in tscat_catalogues():
        assert member_uuids(project.catalogue(catalogue.uuid)) == tscat_member_uuids(catalogue)


def test_a_missing_tscat_is_a_clear_import_error(project, monkeypatch):
    import builtins

    real_import = builtins.__import__

    def without_tscat(name, *arguments, **keywords):
        if name == "tscat":
            raise ImportError("no tscat")
        return real_import(name, *arguments, **keywords)

    monkeypatch.setattr(builtins, "__import__", without_tscat)
    with pytest.raises(ImportError, match=r"cocat-rs\[tscat\]"):
        cc_tscat.import_database(project)


def test_tscats_json_export_imports(filled, project, tmp_path):
    path = tmp_path / "tscat.json"
    catalogues = [tscat_catalogue(name=name) for name in ("listed", "smart")]
    path.write_text(tscat_export_json(catalogues))
    with pytest.warns(UserWarning, match="smart"):
        cc_tscat.import_json(project, path)
    for catalogue in catalogues:
        ours = project.catalogue(catalogue.uuid)
        assert ours.rule is None
        assert member_uuids(ours) == tscat_member_uuids(catalogue)
        for event in tscat.get_events(catalogue)[0]:
            assert_event_matches(project, event)


def test_our_json_export_imports_into_tscat(database, project, tmp_path):
    event = project.create_event(
        start=datetime.datetime(2021, 3, 1, 12, tzinfo=UTC),
        stop=datetime.datetime(2021, 3, 1, 13, 30, 0, 250000, tzinfo=UTC),
        author="dana",
        tags=["shock", "bow"],
        products=["amda/b"],
        confidence=0.4,
        attributes={"n": 2, "seen": datetime.datetime(2021, 3, 2, tzinfo=UTC), "ok": True},
    )
    plain = project.create_event(
        start=datetime.datetime(2021, 3, 2, tzinfo=UTC),
        stop=datetime.datetime(2021, 3, 2, 1, tzinfo=UTC),
        author="erin",
    )
    catalogue = project.create_catalogue(
        name="ours", author="dana", tags=["x"], attributes={"mission": "MMS"}, events=[event]
    )
    smart = project.create_catalogue(name="by erin")
    smart.rule = {"filter": {"all": True}, "where": {"cmp": ["author", "==", "erin"]}}
    path = tmp_path / "ours.json"
    cc_tscat.export_json(project, [catalogue, "by erin"], path)

    imported = tscat_import_json_file(str(path))

    assert [c.uuid for c in imported] == [catalogue.uuid, smart.uuid]
    theirs = tscat_catalogue(uuid=catalogue.uuid)
    assert (theirs.name, theirs.author, theirs.tags) == ("ours", "dana", ["x"])
    assert theirs.variable_attributes() == {"mission": "MMS"}
    assert tscat_member_uuids(theirs) == {event.uuid}
    assert tscat_member_uuids(tscat_catalogue(uuid=smart.uuid)) == {plain.uuid}
    exported = {e.uuid: e for e in tscat.get_events()}
    assert exported[event.uuid].rating == 4
    assert exported[plain.uuid].rating is None
    for uuid in (event.uuid, plain.uuid):
        assert_event_matches(project, exported[uuid])
    assert json.loads(path.read_text())["catalogues"][1]["predicate"] is None


def test_shapes_travel_through_tscat_and_back(database, project, tmp_path):
    """tscat keeps a shape and a shape axis as attributes, so its own export carries them
    back to us."""
    shape = [
        (datetime.datetime(2021, 3, 1, 12, tzinfo=UTC), 10.0),
        (datetime.datetime(2021, 3, 1, 12, 30, tzinfo=UTC), 90.5),
        (datetime.datetime(2021, 3, 1, 13, tzinfo=UTC), 20.0),
    ]
    event = project.create_event(
        start=shape[0][0], stop=shape[-1][0], author="dana", shape=shape
    )
    catalogue = project.create_catalogue(name="pitch", author="dana", events=[event])
    catalogue.shape_axis = ("pitch angle", "deg")
    ours = tmp_path / "ours.json"
    cc_tscat.export_json(project, [catalogue], ours)

    [theirs] = tscat_import_json_file(str(ours))
    back = tmp_path / "back.json"
    back.write_text(tscat_export_json([theirs]))
    again = cc.Project.open(str(tmp_path / "again"), "75ca7000-0000-4000-8000-0000000000aa")
    cc_tscat.import_json(again, back)

    assert again.event(event.uuid).shape == event.shape
    assert again.catalogue(catalogue.uuid).shape_axis == ("pitch angle", "deg")


def test_in_catalogue_keeps_its_catalogue_whatever_tscat_reads_back(database, project):
    """tscat 0.5.1 resolves InCatalogue through a lookup that returns its first catalogue;
    the import reads the stored predicate, so the rule names the catalogue it was made for."""
    tscat.create_catalogue("first", author="a")
    target = tscat.create_catalogue("target", author="a")
    tscat.create_catalogue("smart", author="a", predicate=f.InCatalogue(target))
    tscat.save()
    target_uuid = target.uuid

    cc_tscat.import_database(project)

    smart = next(c for c in project.catalogues() if c.name == "smart")
    assert json.dumps(smart.rule).count(target_uuid) == 1


def test_room_is_an_unknown_keyword_of_the_tscat_functions(tmp_path):
    with pytest.raises(TypeError):
        cc_tscat.import_json(room=None, path=tmp_path / "tscat.json")
