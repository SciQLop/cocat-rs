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
