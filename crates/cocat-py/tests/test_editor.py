"""What a smart-catalogue editor needs: the builder's vocabulary, structured errors,
searches with a source, a preview that saves nothing, and rows read back from a rule."""

import datetime

import pytest

import cocat_rs as cc

UTC = datetime.timezone.utc
BASE = datetime.datetime(2025, 1, 31, 10, 0, tzinfo=UTC)


def at(seconds):
    return BASE + datetime.timedelta(seconds=seconds)


@pytest.fixture
def project(cache):
    return cc.Project.open(cache, "5a5a5a5a-0000-4000-8000-0000000000ab")


@pytest.fixture
def events(project):
    """A short shock by alice, a long wave by bob, both in "Picked" only the wave."""
    shock = project.create_event(start=at(0), stop=at(1), author="alice", tags=["shock"])
    wave = project.create_event(start=at(10), stop=at(3610), author="bob", confidence=0.9)
    picked = project.create_catalogue(name="Picked", events=[wave])
    return shock, wave, picked


def test_the_fields_come_with_their_kind_and_label():
    fields = cc.query_fields()
    assert fields[0] == {"name": "start", "kind": "time", "label": "Start"}
    by_name = {field["name"]: field for field in fields}
    assert by_name["created"]["label"] == "Added"
    assert by_name["tag"]["kind"] == "tags"
    assert by_name["status"]["kind"] == "choice"
    assert by_name["status"]["values"] == [
        "preliminary", "confirmed", "reviewed", "final", "rejected",
    ]


def test_each_kind_offers_its_tests_in_the_web_words():
    assert cc.query_tests("tags") == [
        {"test": "is", "label": "has", "needs_value": True},
        {"test": "is_not", "label": "does not have", "needs_value": True},
    ]
    assert cc.query_tests("time")[0] == {
        "test": "in_last_days", "label": "is in the last … days", "needs_value": True,
    }
    assert cc.query_tests("presence")[1] == {
        "test": "is_not_set", "label": "has none", "needs_value": False,
    }
    with pytest.raises(ValueError, match="kind is one of text, number"):
        cc.query_tests("colour")


def test_rows_write_the_search_text():
    assert cc.clause("confidence", "at_least", "0.8") == "confidence:>=0.8"
    assert cc.clause("tag", "is_not", "shock") == "NOT tag:shock"
    assert cc.clause("created", "in_last_days", "7") == "created:now-7d"
    assert cc.clause("confidence", "is_set") == "_exists_:confidence"
    rows = ["tag:shock", "NOT status:final"]
    assert cc.search_text(rows) == "tag:shock NOT status:final"
    assert cc.search_text(rows, any=True) == "(tag:shock) OR (NOT status:final)"
    with pytest.raises(ValueError, match="tag takes is, is_not"):
        cc.clause("tag", "at_least", "3")
    with pytest.raises(ValueError, match="needs a value"):
        cc.clause("tag", "is", " ")
    with pytest.raises(ValueError, match="no field"):
        cc.clause("colour", "is", "red")


def test_a_search_that_does_not_read_says_where_and_what_was_meant(project):
    with pytest.raises(cc.QueryError) as raised:
        project.search("tag:x auhtor:alice")
    error = raised.value
    assert isinstance(error, ValueError)
    assert (error.position, error.length) == (6, 6)
    assert error.suggestion == "author"
    assert error.message == "there is no field “auhtor”: did you mean “author”?"
    assert str(error) == error.message + " (at character 7)"
    with pytest.raises(cc.QueryError) as raised:
        cc.parse_query("tag:x OR")
    assert raised.value.length is None and raised.value.suggestion is None


def test_parse_query_in_a_project_names_its_catalogues_and_takes_a_source(project, events):
    shock, wave, picked = events
    rule = project.parse_query("catalogue:Picked")
    assert rule == {"filter": {"all": True}, "where": {"in_catalogue": picked.uuid}}
    rule = project.parse_query("author:bob", source=picked)
    assert rule["filter"] == {"catalogue": picked.uuid}
    assert project.parse_query("", source=picked.uuid)["filter"] == {"catalogue": picked.uuid}
    with pytest.raises(cc.QueryError):
        project.parse_query("catalogue:Nowhere")


def test_a_catalogue_takes_a_search_of_another_and_says_which(project, events):
    shock, wave, picked = events
    smart = project.create_catalogue(name="smart")
    assert smart.query_source is None
    smart.set_query("author:bob", source=picked)
    assert smart.query == "author:bob"
    assert smart.query_source == picked
    assert smart.events == {wave}
    smart.set_query("author:alice")
    assert smart.query_source is None
    assert smart.events == {shock}
    smart.rule = {"merge": {"all": True}, "gap": 10}
    assert smart.query_source is None


def test_a_preview_counts_and_lists_the_first_events_without_saving(project, events):
    shock, wave, picked = events
    before = {catalogue.uuid for catalogue in project.catalogues()}
    found = project.preview("duration:<1h OR author:bob")
    assert found["count"] == 2
    assert found["uuid"] == [shock.uuid, wave.uuid]
    assert len(found["start"]) == 2
    first = project.preview("", limit=1)
    assert (first["count"], first["uuid"]) == (2, [shock.uuid])
    assert project.preview("", source=picked)["uuid"] == [wave.uuid]
    rule = {"merge": {"all": True}, "gap": 3600}
    merged = project.preview(rule)
    assert merged["count"] == 1 and merged["uuid"] == [None]
    with_attributes = project.preview("tag:shock", attributes=True)
    assert with_attributes["tags"] == [["shock"]]
    assert {catalogue.uuid for catalogue in project.catalogues()} == before
    with pytest.raises(ValueError, match="source"):
        project.preview(rule, source=picked)
    with pytest.raises(cc.QueryError):
        project.preview("auhtor:x")


def every_row():
    """A row for each test of each kind of field, with a value it takes."""
    values = {
        "text": "alice smith", "number": "0.8", "time": "2015-03-01", "duration": "10min",
        "choice": "reviewed", "tags": "shock", "catalogue": "Picked", "presence": "",
    }
    special = {"in_last_days": "7", "matches": "^al"}
    for field in cc.query_fields() + [{"name": "#density", "kind": "attribute"}]:
        for test in cc.query_tests(field["kind"]):
            value = values.get(field["kind"], "12") if test["needs_value"] else ""
            yield {"field": field["name"], "test": test["test"],
                   "value": special.get(test["test"], value)}


def test_every_row_reads_back_from_its_rule(project, events):
    for row in every_row():
        rule = project.parse_query(cc.clause(**row))
        assert project.builder_rows(rule) == (False, [row]), row


def test_rows_read_back_with_all_or_any_and_rich_searches_have_none(project, events):
    shock, wave, picked = events
    rows = [
        {"field": "tag", "test": "is", "value": "shock"},
        {"field": "catalogue", "test": "is_not", "value": "Picked"},
        {"field": "created", "test": "in_last_days", "value": "7"},
    ]
    for any in (False, True):
        text = cc.search_text([cc.clause(**row) for row in rows], any=any)
        rule = project.parse_query(text, source=picked)
        assert project.builder_rows(rule) == (any, rows)
        assert cc.builder_rows(rule)[1][1]["value"] == picked.uuid
    assert project.builder_rows(project.parse_query("tag:a (tag:b OR tag:c)")) is None
    assert project.builder_rows({"merge": {"all": True}, "gap": 10}) is None
    assert project.builder_rows(project.parse_query("")) == (False, [])


def test_a_search_reads_back_as_it_was_typed(project, events):
    shock, wave, picked = events
    smart = project.create_smart_catalogue("Late", "start:>2018")
    assert smart.query == "start:>2018"
    smart.query = "start:<2020"
    assert smart.query == "start:<2020"
    smart.set_query("tag:shock start:>2018", source=picked)
    assert smart.query == "tag:shock start:>2018"
    smart.rule = smart.rule
    assert smart.query == "tag:shock start:>=2019-01-01", "a rule set as a rule is printed"


def test_a_public_rule_still_has_its_text(project):
    smart = project.create_catalogue("Old")
    smart.rule = {"filter": {"public": True}, "where": project.parse_query("start:<2020")["where"]}
    assert smart.searches_public
    assert smart.query == "start:<2020-01-01"
