"""Public catalogues: every account reads them, curators write them."""

import time

import pytest
from conftest import PASSWORD, login, run

import cocat_rs as cc

READ_ONLY = r"this project is read-only for you \(viewer\)"


def account(server, email, *extra):
    database = str(server.directory / "cocat.db")
    run("create-user", "--email", email, "--password", PASSWORD, "--db_path", database)
    for command in extra:
        run(command, "--email", email, "--db_path", database)
    return login(server.url, email)


def connected(directory, project_id, url, cookie):
    directory.mkdir()
    project = cc.Project.open(str(directory), project_id)
    project.connect(url, cookie)
    project.wait_synced(15)
    return project


def wait_until(predicate, timeout=15.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.02)
    raise AssertionError("the condition did not hold in time")


def test_only_a_curator_creates_one_and_everyone_lists_it(server):
    with pytest.raises(cc.Forbidden):
        cc.raw.create_public_catalogue(server.url, server.cookie, "Mine")
    curator = account(server, "curator@example.org", "set-curator")

    made = cc.raw.create_public_catalogue(
        server.url,
        curator,
        "Shocks",
        folder="AMDA/Solar wind",
        source="AMDA",
        description="Shocks seen by ACE",
        key="ace.shocks_22",
    )

    assert made == "public.ace.shocks_22"
    with pytest.raises(cc.Conflict):
        cc.raw.create_public_catalogue(server.url, curator, "Again", key="ace.shocks_22")
    [entry] = cc.raw.public_catalogues(server.url, server.cookie)
    assert entry["id"] == made
    assert entry["name"] == "Shocks"
    assert entry["folder"] == "AMDA/Solar wind"
    assert entry["source"] == "AMDA"
    assert entry["description"] == "Shocks seen by ACE"
    assert entry["role"] == "viewer"
    assert cc.raw.public_catalogues(server.url, curator)[0]["role"] == "editor"
    assert made not in [info.id for info in cc.raw.projects(server.url, server.cookie)]


def test_a_curator_writes_and_a_reader_gets_a_read_only_copy(server, tmp_path):
    curator = account(server, "curator@example.org", "set-curator")
    public_id = cc.raw.create_public_catalogue(server.url, curator, "Shocks")

    writer = connected(tmp_path / "curator", public_id, server.url, curator)
    assert writer.role == "editor"
    assert not writer.read_only
    writer.create_catalogue(name="ACE shocks")
    writer.save(15)
    writer.close()

    reader = connected(tmp_path / "reader", public_id, server.url, server.cookie)
    wait_until(lambda: [c.name for c in reader.catalogues()] == ["ACE shocks"])
    assert reader.role == "viewer"
    assert reader.read_only
    with pytest.raises(RuntimeError, match=READ_ONLY):
        reader.create_catalogue(name="mine")
    reader.close()


def test_nobody_shares_one(server):
    curator = account(server, "curator@example.org", "set-curator")
    public_id = cc.raw.create_public_catalogue(server.url, curator, "Shocks")

    for cookie in (server.cookie, curator):
        with pytest.raises(cc.Conflict):
            cc.raw.members(server.url, cookie, public_id)
        with pytest.raises(cc.Conflict):
            cc.raw.invite(server.url, cookie, public_id, role="viewer")


def test_a_curator_changes_what_the_index_says_and_it_counts_the_events(server, tmp_path):
    curator = account(server, "curator@example.org", "set-curator")
    public_id = cc.raw.create_public_catalogue(
        server.url, curator, "Shocks", folder="AMDA", source="AMDA", description="d"
    )
    with pytest.raises(cc.Forbidden):
        cc.raw.update_public_catalogue(server.url, server.cookie, public_id, name="mine")
    with pytest.raises(cc.ServerNotFound):
        cc.raw.update_public_catalogue(server.url, curator, "public.nope", name="x")

    cc.raw.update_public_catalogue(
        server.url, curator, public_id, name="ACE shocks", folder="AMDA/Solar wind", description=""
    )

    [entry] = cc.raw.public_catalogues(server.url, server.cookie)
    assert entry["name"] == "ACE shocks"
    assert entry["folder"] == "AMDA/Solar wind"
    assert entry["source"] == "AMDA"
    assert entry["description"] is None
    assert entry["count"] == 0

    writer = connected(tmp_path / "curator", public_id, server.url, curator)
    kept = writer.create_event("2020-01-01T00:00:00", "2020-01-01T01:00:00")
    gone = writer.create_event("2020-01-02T00:00:00", "2020-01-02T01:00:00")
    writer.create_catalogue(name="ACE shocks", events=[kept, gone])
    gone.delete()
    writer.save(15)
    writer.close()
    wait_until(lambda: cc.raw.public_catalogues(server.url, server.cookie)[0]["count"] == 1)


def test_a_smart_catalogue_searching_the_public_catalogues_lists_their_events(
    server, project, tmp_path
):
    from conftest import free_port

    curator = account(server, "keeper@example.org", "set-curator")
    shocks = cc.raw.create_public_catalogue(server.url, curator, "Shocks", key="zz.shocks")
    public = connected(tmp_path / "keeper", shocks, server.url, curator)
    events = [
        public.create_event("2020-01-02T00:00:00Z", "2020-01-02T01:00:00Z", tags=["shock"],
                            attributes={"mach": 3.5}),
        public.create_event("2020-01-03T00:00:00Z", "2020-01-03T01:00:00Z", tags=["wave"]),
    ]
    public.create_catalogue("Shocks", events=events)
    public.save(15)
    public.close()

    found = project.search_public('tag:shock catalogue:"Shocks"')
    assert found["sources"] == [[events[0].uuid]]
    assert found["catalogue"] == [shocks]
    assert found["attributes"] == [{"mach": 3.5}]

    picked = project.create_event("2019-01-01T00:00:00Z", "2019-01-01T01:00:00Z")
    smart = project.create_smart_catalogue("Shocks everywhere", "tag:shock", public=True)
    smart.add_events([picked])
    arrays = smart.event_arrays(attributes=True)
    assert arrays["uuid"] == [picked.uuid, None], "the picked event, then the public one"
    assert arrays["catalogue"] == [None, shocks]
    assert arrays["sources"] == [None, [events[0].uuid]]

    project.connect(f"http://127.0.0.1:{free_port()}", server.cookie)
    offline = smart.event_arrays()
    assert offline["catalogue"] == [None, shocks], "the last answer, while the server is away"
