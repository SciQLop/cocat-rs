"""Submissions to the public catalogues, end to end: an author offers, a curator decides."""

import pytest
from conftest import PASSWORD, login, run
from test_public import connected, wait_until

import cocat_rs as cc


def account(server, email, *extra):
    database = str(server.directory / "cocat.db")
    run("create-user", "--email", email, "--password", PASSWORD, "--db_path", database)
    for command in extra:
        run(command, "--email", email, "--db_path", database)
    return login(server.url, email)


def events_of(directory, project_id, server, cookie):
    project = connected(directory, project_id, server.url, cookie)
    [catalogue] = project.catalogues()
    found = {event.uuid: event.confidence for event in catalogue.events}
    name = catalogue.name
    project.close()
    return name, found


def test_a_new_catalogue_becomes_public_once_a_curator_accepts_it(server, tmp_path):
    curator = account(server, "curator@example.org", "set-curator")
    mine = connected(tmp_path / "mine", server.project_id, server.url, server.cookie)
    waves = [
        mine.create_event(f"2020-01-0{day}T00:00:00", f"2020-01-0{day}T01:00:00") for day in (1, 2)
    ]
    catalogue = mine.create_catalogue(name="Waves", events=waves)
    mine.save(15)
    mine.close()

    submitted = cc.submit_catalogue(
        server.url,
        server.cookie,
        server.project_id,
        catalogue.uuid,
        folder="Lab/Waves",
        description="Two waves",
        note="Seen by MMS",
    )
    [pending] = cc.submissions(server.url, curator)
    with pytest.raises(cc.Forbidden):
        cc.review_submission(server.url, server.cookie, submitted, True)
    cc.review_submission(server.url, curator, submitted, True, note="Welcome")

    assert pending["id"] == submitted
    assert pending["status"] == "pending"
    assert pending["title"] == "Waves"
    assert pending["note"] == "Seen by MMS"
    [entry] = cc.public_catalogues(server.url, server.cookie)
    assert entry["name"] == "Waves"
    assert entry["folder"] == "Lab/Waves"
    assert entry["description"] == "Two waves"
    name, found = events_of(tmp_path / "public", entry["id"], server, server.cookie)
    assert name == "Waves"
    assert set(found) == {event.uuid for event in waves}
    [mine_now] = cc.submissions(server.url, server.cookie, mine=True)
    assert mine_now["status"] == "accepted"
    assert mine_now["target"] == entry["id"]


def test_a_proposed_change_replaces_the_public_catalogue_once_accepted(server, tmp_path):
    curator = account(server, "curator@example.org", "set-curator")
    public_id = cc.create_public_catalogue(server.url, curator, "Shocks")
    keeper = connected(tmp_path / "curator", public_id, server.url, curator)
    kept = keeper.create_event("2020-01-01T00:00:00", "2020-01-01T01:00:00")
    dropped = keeper.create_event("2020-01-02T00:00:00", "2020-01-02T01:00:00")
    keeper.create_catalogue(name="Shocks", events=[kept, dropped])
    keeper.save(15)
    keeper.close()

    proposed = cc.propose_change(server.url, server.cookie, public_id)
    copy = connected(tmp_path / "copy", proposed["project_id"], server.url, server.cookie)
    wait_until(lambda: len(copy.catalogues()) == 1)
    catalogue = copy.catalogue(proposed["catalogue"])
    copy.event(kept.uuid).confidence = 0.75
    added = copy.create_event("2020-01-03T00:00:00", "2020-01-03T01:00:00")
    catalogue.add_events([added])
    catalogue.remove_events([dropped.uuid])
    copy.save(15)
    copy.close()
    submitted = cc.submit_update(
        server.url, server.cookie, proposed["project_id"], proposed["catalogue"], note="fixed"
    )
    cc.review_submission(server.url, curator, submitted, True)

    name, found = events_of(tmp_path / "reader", public_id, server, server.cookie)
    assert name == "Shocks"
    assert found == {kept.uuid: 0.75, added.uuid: None}
    [decided] = cc.submissions(server.url, server.cookie, mine=True)
    assert decided["kind"] == "update"
    assert decided["status"] == "accepted"


def test_a_declined_submission_tells_its_author_why(server, tmp_path):
    curator = account(server, "curator@example.org", "set-curator")
    stranger = account(server, "stranger@example.org")
    mine = connected(tmp_path / "mine", server.project_id, server.url, server.cookie)
    catalogue = mine.create_catalogue(name="Waves")
    mine.save(15)
    mine.close()
    first = cc.submit_catalogue(server.url, server.cookie, server.project_id, catalogue.uuid)
    second = cc.submit_catalogue(server.url, server.cookie, server.project_id, catalogue.uuid)

    cc.review_submission(server.url, curator, first, False, note="Add a description")
    with pytest.raises(cc.ServerNotFound):
        cc.withdraw_submission(server.url, stranger, second)
    cc.withdraw_submission(server.url, server.cookie, second)
    with pytest.raises(cc.Conflict):
        cc.review_submission(server.url, curator, second, True)

    statuses = {s["id"]: s for s in cc.submissions(server.url, server.cookie, mine=True)}
    assert statuses[first]["status"] == "declined"
    assert statuses[first]["review_note"] == "Add a description"
    assert statuses[first]["reviewer"] == "a curator"
    assert statuses[second]["status"] == "withdrawn"
    assert cc.submissions(server.url, stranger) == []
    assert cc.public_catalogues(server.url, server.cookie) == []


def test_a_public_catalogue_copies_into_a_project_to_use_it(server, tmp_path):
    curator = account(server, "curator@example.org", "set-curator")
    public_id = cc.create_public_catalogue(server.url, curator, "Shocks")
    keeper = connected(tmp_path / "curator", public_id, server.url, curator)
    shock = keeper.create_event("2020-01-01T00:00:00", "2020-01-01T01:00:00")
    keeper.create_catalogue(name="ACE shocks", events=[shock])
    keeper.save(15)
    keeper.close()

    into_new = cc.copy_public_catalogue(server.url, server.cookie, public_id)
    into_mine = cc.copy_public_catalogue(
        server.url, server.cookie, public_id, project_id=server.project_id
    )
    with pytest.raises(cc.Conflict):
        cc.copy_public_catalogue(server.url, server.cookie, public_id, server.project_id)

    assert into_new["name"] == "Shocks"
    assert into_mine["project_id"] == server.project_id
    assert into_mine["catalogue"] == into_new["catalogue"]
    name, found = events_of(tmp_path / "copy", into_new["project_id"], server, server.cookie)
    assert name == "ACE shocks"
    assert set(found) == {shock.uuid}
    with pytest.raises(cc.ServerNotFound):
        cc.submit_update(server.url, server.cookie, into_new["project_id"], into_new["catalogue"])
