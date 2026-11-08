import datetime

import numpy as np
import pytest

import cocat_rs as cc

UTC = datetime.timezone.utc
BASE = datetime.datetime(2025, 1, 31, 10, 0, tzinfo=UTC)


def at(seconds):
    return BASE + datetime.timedelta(seconds=seconds)


@pytest.fixture
def project(cache):
    return cc.Project.open(cache, "5a5a5a5a-0000-4000-8000-000000000000")


@pytest.fixture
def events(project):
    """Three one-second events, ten seconds apart; the second is by bob."""
    return [
        project.create_event(
            start=at(10 * n),
            stop=at(10 * n + 1),
            author="bob" if n == 2 else "alice",
            confidence=n / 10,
        )
        for n in (1, 2, 3)
    ]


def test_a_catalogue_has_no_rule_until_given_one(project):
    catalogue = project.create_catalogue(name="plain")
    assert catalogue.rule is None
    rule = {"filter": {"all": True}, "where": {"cmp": ["author", "==", "bob"]}}
    catalogue.rule = rule
    assert catalogue.rule == rule
    catalogue.rule = None
    assert catalogue.rule is None


def test_a_rule_yields_members_besides_the_listed_events(project, events):
    smart = project.create_catalogue(name="smart", events=[events[0]])
    smart.rule = {"filter": {"all": True}, "where": {"cmp": ["author", "==", "bob"]}}
    assert smart.events == {events[0], events[1]}
    arrays = smart.event_arrays()
    assert arrays["uuid"] == [events[0].uuid, events[1].uuid]
    assert arrays["sources"] == [None, None]


@pytest.mark.parametrize(
    ("rule", "message"),
    [
        (
            {"filter": {"all": True}, "where": {"cmp": ["start", "=~", 1]}},
            "rule.where.cmp: unknown operator '=~'",
        ),
        (
            {"union": [{"all": True}, {"tile": {"all": True}}]},
            "rule.union[1]: 'tile' needs 'width'",
        ),
        (
            {"filter": {"all": True}, "where": {"has": "colour"}},
            "rule.where.has: colour is neither an event field nor an attribute (#key)",
        ),
    ],
)
def test_a_malformed_rule_says_what_is_wrong_and_where(project, rule, message):
    catalogue = project.create_catalogue(name="smart")
    with pytest.raises(ValueError) as raised:
        catalogue.rule = rule
    assert str(raised.value) == message
    assert catalogue.rule is None


def test_a_rule_that_is_not_json_is_refused(project):
    catalogue = project.create_catalogue(name="smart")
    with pytest.raises(ValueError):
        catalogue.rule = {"all": object()}


def test_derived_intervals_have_no_uuid_and_name_their_sources(project, events):
    smart = project.create_catalogue(name="merged")
    smart.rule = {"merge": {"all": True}, "gap": 10}
    arrays = smart.event_arrays(attributes=True)
    assert arrays["uuid"] == [None]
    assert sorted(arrays["sources"][0]) == sorted(event.uuid for event in events)
    assert arrays["start"][0] == np.datetime64(at(10).replace(tzinfo=None), "us")
    assert arrays["stop"][0] == np.datetime64(at(31).replace(tzinfo=None), "us")
    assert arrays["attributes"] == [{}]
    assert smart.event_attributes() == [{}]
    assert smart.events == set()


def test_materialize_makes_a_static_catalogue_of_the_current_members(project, events):
    smart = project.create_catalogue(name="smart", events=[events[0]])
    smart.rule = {
        "union": [
            {"filter": {"all": True}, "where": {"cmp": ["author", "==", "bob"]}},
            {"shift": {"catalogue": smart.uuid + "-none"}, "by": 1},
            {"merge": {"filter": {"all": True}, "where": {"cmp": ["confidence", ">", 0.25]}}, "gap": 0},
        ]
    }
    copy = smart.materialize("snapshot")
    assert copy.name == "snapshot"
    assert copy.rule is None
    assert copy.uuid != smart.uuid
    listed = copy.event_arrays()
    assert listed["sources"] == [None, None, None]
    made = [event for event in copy.events if event not in (events[0], events[1])]
    assert len(made) == 1
    assert (made[0].start, made[0].stop) == (at(30), at(31))
    origin = project.origin(made[0].origin)
    assert origin["kind"] == "rule"
    assert origin["derived_from"] == smart.uuid


def test_a_rule_evaluation_error_is_a_value_error(project):
    smart = project.create_catalogue(name="loop")
    smart.rule = {"catalogue": smart.uuid}
    with pytest.raises(ValueError, match="again through itself"):
        smart.event_arrays()


def test_a_tscat_predicate_becomes_a_rule():
    predicate = {
        "type": "Comparison",
        "op": ">",
        "lhs": {"type": "Field", "name": "rating"},
        "rhs": 5,
    }
    assert cc.rule_from_tscat(predicate) == {
        "filter": {"all": True},
        "where": {"cmp": ["confidence", ">", 0.5]},
    }


def test_a_tscat_predicate_that_does_not_convert_is_a_value_error():
    with pytest.raises(ValueError):
        cc.rule_from_tscat({"type": "Xor"})


def test_tags_products_and_author_of_an_event_change(project, events):
    event = events[0]
    event.author = "carol"
    event.tags = ["b", "a"]
    event.products = ["speasy/x"]
    assert (event.author, event.tags, event.products) == ("carol", {"a", "b"}, {"speasy/x"})
    event.author = None
    assert event.author is None


def test_the_tags_of_a_catalogue_change(project):
    catalogue = project.create_catalogue(name="tagged", tags=["x"])
    catalogue.tags = ["y", "z"]
    assert catalogue.tags == {"y", "z"}
