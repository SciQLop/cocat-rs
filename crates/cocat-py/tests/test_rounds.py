"""Selection rounds end to end: proposers write intervals, voters pick, a decider decides."""

import pytest
from conftest import PASSWORD, run
from test_public import wait_until

import cocat_rs as cc
from cocat_rs import client as client_module

BUDGET = {
    "cocat:attribute": "mode",
    "cocat:cap": {"qudt:numericValue": 10, "qudt:hasUnit": {"skos:notation": "Gibyte"}},
    "cocat:rates": [
        {
            "cocat:value": "burst",
            "cocat:rate": {
                "qudt:numericValue": 5,
                "qudt:hasUnit": {"skos:notation": "Mibyte/min"},
            },
        }
    ],
}


@pytest.fixture(autouse=True)
def fresh_sessions(monkeypatch):
    monkeypatch.setattr(client_module, "_MEMORY", {})


def signed_in(server, email):
    """A client signed in as a new editor of the server's project."""
    database = str(server.directory / "cocat.db")
    run("create-user", "--email", email, "--password", PASSWORD, "--db_path", database)
    run(
        "add-user-to-room",
        *("--email", email, "--room_id", server.project_id, "--role", "editor"),
        *("--db_path", database),
    )
    client = cc.Client(server.url)
    client.login(email, PASSWORD)
    return client


def owner_client(server):
    client = cc.Client(server.url)
    client.login(server.email, PASSWORD)
    return client


def propose(client, project_id, round_id, cache, mode):
    proposal = client.my_proposal(project_id, round_id, cache=cache)
    event = proposal.create_event(
        "2020-01-01T00:00:00", "2020-01-01T01:00:00", attributes={"mode": mode}
    )
    proposal.save(15)
    proposal.close()
    return event.uuid


def test_a_round_from_proposal_to_decision(server, tmp_path):
    ada = signed_in(server, "ada@example.org")
    bea = signed_in(server, "bea@example.org")
    owner = owner_client(server)
    project = server.project_id
    made = owner.create_round(project, "Round 1", budget=BUDGET)
    round_id = made["id"]
    owner.set_round_members(
        project,
        round_id,
        proposers=["ada@example.org", "bea@example.org"],
        voters=["ada@example.org", "bea@example.org"],
        deciders=[server.email],
    )

    ada_event = propose(ada, project, round_id, tmp_path / "ada", "burst")
    bea_event = propose(bea, project, round_id, tmp_path / "bea", "survey")
    owner.move_round(project, round_id, "voting")
    ada.vote(project, round_id, [bea_event])
    bea.vote(project, round_id, [ada_event, bea_event])

    final = owner.open(project, cache=tmp_path / "owner")
    chosen = [
        final.create_event(
            "2020-01-01T00:00:00",
            "2020-01-01T01:00:00",
            attributes={"mode": "burst", "proposed_by": email},
        )
        for email in ("ada@example.org", "bea@example.org")
    ]
    catalogue = final.create_catalogue(name="Selected", events=chosen)
    final.save(15)
    decided = owner.move_round(project, round_id, "decided", catalogue.uuid)
    wait_until(lambda: "round" in catalogue.attributes)

    assert decided["state"] == "decided"
    assert decided["final_catalogue"] == catalogue.uuid
    assert {vote["voter"]: len(vote["events"]) for vote in owner.votes(project, round_id)} == {
        "ada@example.org": 1,
        "bea@example.org": 2,
    }
    assert "Round 1" in catalogue.attributes["round"]
    final.close()


def test_a_blind_proposal_cannot_be_opened_by_another_proposer(server, tmp_path):
    ada = signed_in(server, "ada@example.org")
    bea = signed_in(server, "bea@example.org")
    owner = owner_client(server)
    project = server.project_id
    round_id = owner.create_round(project, "Blind", visibility="blind")["id"]
    owner.set_round_members(
        project,
        round_id,
        proposers=["ada@example.org", "bea@example.org"],
        voters=[],
        deciders=[server.email],
    )
    ada.my_proposal(project, round_id, cache=tmp_path / "ada").close()
    [ada_proposal] = [p for p in owner.round(project, round_id)["proposals"] if p["doc"]]

    with pytest.raises(cc.Forbidden):
        bea.open(f"{project}/{ada_proposal['doc']}", cache=tmp_path / "bea")


def test_round_cost_matches_the_server_budget(server, tmp_path):
    owner = owner_client(server)
    made = owner.create_round(server.project_id, "Budgeted", budget=BUDGET)
    local = cc.Project.open(str(tmp_path), "local")
    hour = ("2020-01-01T00:00:00", "2020-01-01T01:00:00")
    burst = local.create_event(*hour, attributes={"mode": "burst"})
    unpriced = {"uuid": "x", "start": hour[0], "stop": hour[1], "attributes": {"mode": "other"}}

    cost = cc.round_cost([burst, unpriced], made["budget"])

    assert cost["total"] == pytest.approx(300 / 1024)
    assert cost["cap"] == 10
    assert cost["unit"] == "Gibyte"
    assert cost["unpriced"] == ["x"]
    assert cost["text"] == "0.29 of 10 Gibyte used, 1 interval without a rate"


def detections(owner, tmp_path, count=5):
    project = owner.open(owner.create_project("pipeline").id, cache=tmp_path / "pipeline")
    events = [
        project.create_event(
            f"2020-01-0{day}T00:00:00", f"2020-01-0{day}T01:00:00", attributes={"snr": day}
        )
        for day in range(1, count + 1)
    ]
    catalogue = project.create_catalogue(name="Detections", events=events)
    project.save(15)
    return project, catalogue, events


def test_a_move_carries_a_note(server):
    owner = owner_client(server)
    round_id = owner.create_round(server.project_id, "Noted")["id"]

    moved = owner.move_round(server.project_id, round_id, "voting", note="Looks complete")

    assert moved["log"][-1]["note"] == "Looks complete"


def test_a_pipeline_reviews_its_detections(server, tmp_path):
    ada = signed_in(server, "ada@example.org")
    bea = signed_in(server, "bea@example.org")
    owner = owner_client(server)
    pipeline, catalogue, events = detections(owner, tmp_path)

    made = owner.create_round(
        server.project_id, "Review", kind="review", candidates=catalogue
    )
    ada.verdict(server.project_id, made["id"], events[0], "accept")
    ada.set_verdicts(
        server.project_id, made["id"], {events[1].uuid: "reject", events[2].uuid: "unsure"}
    )
    bea.verdict(server.project_id, made["id"], events[0].uuid, "accept")
    bea.verdict(server.project_id, made["id"], events[1].uuid, "accept")
    bea.verdict(server.project_id, made["id"], events[1].uuid, None)
    answer = owner.verdicts(server.project_id, made["id"])

    assert made["kind"] == "review"
    assert made["state"] == "voting"
    assert made["candidates"]["count"] == 5
    counts = {
        found["event"]: (found["accept"], found["reject"], found["unsure"])
        for found in answer["events"]
    }
    assert counts == {
        events[0].uuid: (2, 0, 0),
        events[1].uuid: (0, 1, 0),
        events[2].uuid: (0, 0, 1),
    }
    assert set(counts) <= {event.uuid for event in events}
    pipeline.close()


def review_of(owner, server, tmp_path):
    pipeline, catalogue, events = detections(owner, tmp_path)
    made = owner.create_round(
        server.project_id, "Review", kind="review", candidates=catalogue
    )
    pipeline.close()
    return made, events


def candidate_events(owner, server, made, tmp_path):
    doc = made["candidates"]["doc"]
    candidates = owner.open(f"{server.project_id}/{doc}", cache=tmp_path / "candidates")
    found = [
        {"uuid": e.uuid, "start": e.start, "stop": e.stop, "attributes": e.attributes}
        for e in sorted(candidates.catalogues()[0].events, key=lambda e: e.start)
    ]
    candidates.close()
    return found


def test_the_review_rules_agree_with_the_verdicts(server, tmp_path):
    owner = owner_client(server)
    made, events = review_of(owner, server, tmp_path)
    project, round_id = server.project_id, made["id"]
    owner.set_verdicts(project, round_id, {events[0].uuid: "accept", events[1].uuid: "reject"})
    rows = cc.round_review.rows(
        [(None, candidate_events(owner, server, made, tmp_path))],
        owner.verdicts(project, round_id),
        server.email,
    )

    assert cc.round_review.progress(rows) == (2, 5, 0)
    assert cc.round_review.next_unreviewed(rows, 0) == 2
    assert cc.round_review.majority(rows) == [True, False, False, False, False]
    final = cc.round_review.final_events(rows, [True, False, False, False, False])
    assert final[0]["attributes"]["candidate"] == events[0].uuid
    assert final[0]["attributes"]["accepts"] == 1
    assert final[0]["uuid"] != events[0].uuid
    assert cc.round_review.final_catalogue_uuid(round_id) == cc.round_review.final_catalogue_uuid(
        round_id
    )


def test_deciding_twice_from_python_keeps_one_catalogue(server, tmp_path):
    owner = owner_client(server)
    made, events = review_of(owner, server, tmp_path)
    project, round_id = server.project_id, made["id"]
    owner.set_verdicts(project, round_id, {e.uuid: "accept" for e in events[:3]})
    team = owner.open(project, cache=tmp_path / "team")
    rows = cc.round_review.rows(
        [(None, candidate_events(owner, server, made, tmp_path))],
        owner.verdicts(project, round_id),
        server.email,
    )
    picked = cc.round_review.majority(rows)

    first = team.decide(round_id, cc.round_review.final_events(rows, picked))
    second = team.decide(round_id, cc.round_review.final_events(rows, picked[:2] + [False] * 3))

    final_uuid = cc.round_review.final_catalogue_uuid(round_id)
    named = [c for c in team.catalogues() if c.uuid == final_uuid]
    assert first["state"] == second["state"] == "decided"
    assert first["final_catalogue"] == final_uuid
    assert len(named) == 1
    assert len(named[0].events) == 2
    team.close()
