import datetime

import pytest

import cocat_rs as cc

UTC = datetime.timezone.utc
BASE = datetime.datetime(2025, 1, 31, 10, 0, tzinfo=UTC)


def at(seconds):
    return BASE + datetime.timedelta(seconds=seconds)


@pytest.fixture
def project(cache):
    return cc.Project.open(cache, "5a5a5a5a-0000-4000-8000-0000000000aa")


@pytest.fixture
def events(project):
    """A short shock by alice, a long wave by bob, and one in the trash."""
    shock = project.create_event(start=at(0), stop=at(1), author="alice", tags=["shock"])
    wave = project.create_event(start=at(10), stop=at(3610), author="bob", confidence=0.9)
    gone = project.create_event(start=at(20), stop=at(21), tags=["shock"])
    gone.delete()
    return shock, wave


def test_search_finds_the_events_a_query_matches_in_time_order(project, events):
    shock, wave = events
    assert project.search("tag:shock") == [shock]
    assert project.search("duration:>=1h OR author:ALI") == [shock, wave]
    assert project.search("") == [shock, wave]
    assert project.search("confidence:>=0.8 NOT tag:shock") == [wave]


def test_a_query_that_does_not_read_is_a_value_error_with_its_position(project):
    with pytest.raises(ValueError) as raised:
        project.search("tag:x auhtor:alice")
    assert str(raised.value) == (
        "there is no field “auhtor”: did you mean “author”? (at character 7)"
    )


def test_a_smart_catalogue_is_made_from_a_query(project, events):
    shock, wave = events
    smart = project.create_smart_catalogue("long", "duration:>=1h")
    assert smart.name == "long"
    assert smart.query == "duration:>=1h"
    assert smart.rule == {
        "filter": {"all": True},
        "where": {"cmp": ["duration", ">=", 3600]},
    }
    assert smart.events == {wave}


def test_a_smart_catalogue_can_search_one_catalogue(project, events):
    shock, wave = events
    source = project.create_catalogue(name="Picked", events=[wave])
    smart = project.create_smart_catalogue("picked by bob", "author:bob", source=source)
    assert smart.rule["filter"] == {"catalogue": source.uuid}
    assert smart.query == "author:bob"
    assert smart.events == {wave}
    by_uuid = project.create_smart_catalogue("again", "author:bob", source=source.uuid)
    assert by_uuid.events == {wave}


def test_the_query_of_a_catalogue_reads_and_sets_its_rule(project, events):
    shock, wave = events
    picked = project.create_catalogue(name="Picked", events=[shock])
    catalogue = project.create_catalogue(name="smart")
    assert catalogue.query is None
    catalogue.query = 'catalogue:Picked OR status:"rejected"'
    assert catalogue.query == "catalogue:Picked OR status:rejected"
    assert catalogue.rule["where"]["or"][0] == {"in_catalogue": picked.uuid}
    assert catalogue.events == {shock}
    catalogue.rule = {"merge": {"all": True}, "gap": 10}
    assert catalogue.query is None
    with pytest.raises(ValueError, match=r"expected a search term \(at character 2\)"):
        catalogue.query = "("


def test_parse_query_gives_the_rule_for_scripts():
    assert cc.parse_query("tag:shock created:>now-7d") == {
        "filter": {"all": True},
        "where": {
            "and": [
                {"contains": ["tags", "shock"]},
                {"cmp": ["created", ">", {"now": "-7d"}]},
            ]
        },
    }
    with pytest.raises(ValueError, match="no catalogue is named"):
        cc.parse_query("catalogue:x")
