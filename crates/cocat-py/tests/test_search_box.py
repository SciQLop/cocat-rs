"""What an event table's search box needs, the web app's filter box: a search tested
against the shown events, the plain-words filter, what the text is, its colours, its
completions and the preset searches."""

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
    return cc.Project.open(cache, "5a5a5a5a-0000-4000-8000-0000000000ac")


@pytest.fixture
def events(project):
    """A short shock by alice, a long wave by bob in "Picked", one shock in the trash."""
    shock = project.create_event(
        start=at(0), stop=at(1), author="alice", tags=["shock"], attributes={"mode": "burst"}
    )
    wave = project.create_event(start=at(10), stop=at(3610), author="bob", confidence=0.9)
    gone = project.create_event(start=at(20), stop=at(21), author="alice", tags=["shock"])
    gone.delete()
    picked = project.create_catalogue(name="Picked", events=[wave])
    return shock, wave, gone, picked


def test_a_search_mask_is_aligned_to_the_shown_events(project, events):
    shock, wave, gone, picked = events
    shown = [wave.uuid, shock, gone.uuid, "5a5a5a5a-0000-4000-8000-0000000000ff"]
    mask = project.matches("author:alice", shown)
    assert isinstance(mask, np.ndarray) and mask.dtype == np.bool_
    assert mask.tolist() == [False, True, False, False]
    assert project.matches("catalogue:Picked", shown).tolist() == [True, False, False, False]
    assert project.matches("", shown, source=picked).tolist() == [True, False, False, False]
    assert project.matches("tag:shock", []).tolist() == []
    with pytest.raises(cc.QueryError):
        project.matches("auhtor:x", shown)


def test_a_derived_interval_is_tested_by_its_range(project, events):
    later = (at(5), at(6))
    assert project.matches("start:>=2025-01-31T10:00:03Z", [later]).tolist() == [True]
    assert project.matches("tag:shock", [later]).tolist() == [False]
    with pytest.raises(ValueError, match="start, stop"):
        project.matches("", [None])


def test_the_mask_is_what_a_smart_catalogue_with_that_search_holds(project, events):
    shock, wave, gone, picked = events
    shown = [shock.uuid, wave.uuid, gone.uuid]
    for search in ["author:alice", "duration:>1h OR tag:shock", "NOT _exists_:catalogue"]:
        smart = project.create_smart_catalogue(search, search)
        held = {event.uuid for event in smart.events}
        assert project.matches(search, shown).tolist() == [u in held for u in shown], search


def test_plain_words_filter_what_the_table_shows(project, events):
    shock, wave, gone, picked = events
    shown = [shock.uuid, wave.uuid, gone, (at(30), at(31))]
    words = lambda text: project.filter_words(text, shown).tolist()
    assert words("ALICE") == [True, False, True, False]
    assert words("burst") == [True, False, False, False]
    assert words("2025-01-31 10:00:1") == [False, True, False, False]
    assert words("2025-01-31 10:00:30") == [False, False, False, True]
    assert words("") == [True, True, True, True]


def test_the_kind_of_text_says_which_filter_applies(project, events):
    assert cc.query_kind("  ") == "empty"
    assert cc.query_kind("shock alice") == "words"
    assert cc.query_kind("tag:shock") == "rule"
    assert cc.query_kind("tag:shock OR") == "broken"
    assert cc.query_kind("tga:shock") == "rule", "an attribute nobody has"
    assert cc.query_kind("catalogue:Picked") == "broken", "no project names it"
    assert project.query_kind("catalogue:Picked") == "rule"
    assert project.query_kind("auhtor:alice") == "broken", "the project's search refuses it"
    assert project.query_kind("mode:burst") == "rule", "an attribute of the project"


def test_pieces_colour_the_text_and_give_it_back_exactly():
    text = '(tag:shock OR confidence:>=0.8)  sheath "a b"'
    pieces = cc.pieces(text)
    assert "".join(piece["text"] for piece in pieces) == text
    assert pieces[0] == {"kind": "bracket", "text": "("}
    assert pieces[1] == {
        "kind": "term", "text": "tag:shock", "field": "tag", "op": ":", "value": "shock",
    }
    assert pieces[3] == {"kind": "keyword", "text": "OR"}
    assert pieces[5]["op"] == ":>="
    assert {piece["kind"] for piece in pieces} == {"bracket", "term", "space", "keyword", "word"}


def test_completion_suggests_from_the_project_and_accepting_replaces_the_word(
    project, events
):
    vocabulary = project.vocabulary()
    assert vocabulary["authors"] == ["alice", "bob"]
    assert vocabulary["catalogues"] == ["Picked"]
    assert vocabulary["attributes"] == ["mode"]
    assert "shock" in vocabulary["tags"]
    text = "confidence:>=0.8 tag:sh status:final"
    found = cc.complete(text, 23, vocabulary)
    assert (found["from"], found["to"]) == (17, 23)
    assert found["suggestions"][0] == {"insert": "tag:shock ", "kind": "tag", "detail": "tag"}
    assert cc.accept(text, found, found["suggestions"][0]) == (
        "confidence:>=0.8 tag:shock status:final", 27,
    )
    assert cc.complete("catalogue:pi", 12, {"catalogues": ["Picked"]})["suggestions"] == [
        {"insert": "catalogue:Picked ", "kind": "catalogue", "detail": "catalogue"}
    ]
    assert cc.complete("tag:shock NOT", 13, vocabulary) is None


def test_the_presets_are_data():
    presets = cc.presets("bob@lab.fr")
    assert presets[0] == {"name": "Recently added", "query": "created:now-7d"}
    assert {"name": "Mine", "query": "author:=bob@lab.fr"} in presets
    assert all(cc.query_kind(preset["query"]) == "rule" for preset in presets)



def test_an_attribute_needs_no_hash(project, events):
    shock, wave, gone, picked = events
    jump = project.create_event(start=at(0), stop=at(1), attributes={"B_jump": 2.0, "status": "x"})
    vocabulary = project.vocabulary()
    found = cc.complete("B", 1, vocabulary)
    assert {"insert": "B_jump:", "kind": "attribute", "detail": "attribute"} in found["suggestions"]
    chosen = next(s for s in found["suggestions"] if s["insert"] == "B_jump:")
    assert cc.accept("B", found, chosen) == ("B_jump:", 7)
    statuses = [s["insert"] for s in cc.complete("statu", 5, vocabulary)["suggestions"]]
    assert statuses == ["status:", "#status:"], "a field's name forces the #"
    shown = [shock.uuid, jump.uuid]
    assert project.matches("B_jump:>1", shown).tolist() == [False, True]
    assert project.matches("#B_jump:>1", shown).tolist() == [False, True]
    assert cc.query_kind("B_jump:>1") == "rule"
    with pytest.raises(cc.QueryError) as error:
        project.parse_query("B_jmp:>1")
    assert error.value.suggestion == "B_jump"


def test_keywords_are_offered_where_they_fit():
    found = cc.complete("tag:a ", 6, {})
    assert found["suggestions"][:3] == [
        {"insert": "OR ", "kind": "keyword", "detail": "keyword"},
        {"insert": "AND ", "kind": "keyword", "detail": "keyword"},
        {"insert": "NOT ", "kind": "keyword", "detail": "keyword"},
    ]
    after_or = [s["insert"] for s in cc.complete("tag:a OR ", 9, {})["suggestions"]]
    assert after_or[0] == "NOT " and "OR " not in after_or and "AND " not in after_or


def test_names_begun_come_first_by_kind_and_say_what_they_are():
    vocabulary = {
        "tags": ["origin_test"],
        "catalogues": ["orbits"],
        "attributes": ["origin", "Orbit No.", "normal_x", "normal_y", "normal_z"],
    }
    text = "tag:a AND Or"
    found = cc.complete(text, len(text), vocabulary)["suggestions"]
    assert [(s["insert"], s["detail"]) for s in found] == [
        ("origin:", "field"),
        ("#origin:", "attribute"),
        ('"Orbit No.":', "attribute"),
        ("tag:origin_test ", "tag"),
        ("catalogue:orbits ", "catalogue"),
    ], "two letters begin a name; normal_x only contains them"
    found = cc.complete("rma", 3, vocabulary)["suggestions"]
    assert [s["insert"] for s in found] == ["normal_x:", "normal_y:", "normal_z:"]
    status = cc.complete("status:f", 8, {})["suggestions"]
    assert status == [{"insert": "status:final ", "kind": "status", "detail": "value"}]


def test_attribute_values_complete_after_the_name(project, events):
    vocabulary = project.vocabulary()
    assert vocabulary["attribute_values"] == {"mode": ["burst"]}
    for text in ["mode:b", "#mode:", '"mode":bu']:
        found = cc.complete(text, len(text), vocabulary)
        name = text.split(":")[0]
        assert found["suggestions"] == [
            {"insert": f"{name}:burst ", "kind": "value", "detail": "value"}
        ], text
    assert cc.complete("mode:b", 6, {"attributes": ["mode"]}) is None, "no values given"


def test_a_true_or_false_attribute_is_found_by_its_value(project):
    """Vibe, 2026-11-26: `flag:true` read, and found nothing, silently."""
    flagged = project.create_event(start=at(0), stop=at(1), attributes={"flag": True})
    project.create_event(start=at(2), stop=at(3), attributes={"flag": False})

    for text in ["flag:true", "flag:TRUE", "flag:=true", "#flag:true"]:
        assert [e.uuid for e in project.search(text)] == [flagged.uuid], text
    assert len(project.search("flag:false")) == 1
    assert len(project.search("NOT flag:true")) == 1
