import datetime
import json
import subprocess
from pathlib import Path

PYTHON_COCAT = Path("/home/jeandet/Documents/prog/cocat-python-tests/.venv/bin/python")
PYTHON_COCAT_ROOT = "/home/jeandet/Documents/prog/cocat-python-tests"
UTC = datetime.timezone.utc


def wait_until(predicate, timeout=15.0):
    import time

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.02)
    raise AssertionError("the condition did not hold in time")


def test_python_cocat_and_cocat_rs_read_each_other(room, other, server, tmp_path):
    catalogue = room.create_catalogue(
        name="alpha",
        author="rust@x.org",
        tags=["t1", "t2"],
        attributes={"mission": "MMS", "level": 2.0},
    )
    event = room.create_event(
        start=datetime.datetime(2025, 1, 31, 10, 0, tzinfo=UTC),
        stop=datetime.datetime(2025, 1, 31, 10, 1, tzinfo=UTC),
        author="rust@x.org",
        tags=["t1"],
        products=["p1"],
        rating=3,
        attributes={"q": 1.5, "flag": True},
    )
    catalogue.add_events([event])

    # Wait until the server has applied the write, so Python cocat cannot pull too early.
    wait_until(
        lambda: any(seen.uuid == catalogue.uuid for seen in other.catalogues())
    )

    output = tmp_path / "cross.json"
    run = subprocess.run(
        [
            str(PYTHON_COCAT),
            str(Path(__file__).parent / "cross_client.py"),
            server.url,
            str(server.url.rsplit(":", 1)[1]),
            server.room_id,
            server.cookie,
            event.uuid,
            str(output),
        ],
        check=False,
        capture_output=True,
        env={"PYTHONPATH": f"{PYTHON_COCAT_ROOT}/src", "PATH": "/usr/bin:/bin"},
        timeout=60,
    )
    assert run.returncode == 0, run.stderr.decode()
    read = json.loads(output.read_text())

    assert read["read"]["catalogue"] == {
        "uuid": catalogue.uuid,
        "name": "alpha",
        "author": "rust@x.org",
        "tags": ["t1", "t2"],
        "attributes": {"mission": "MMS", "level": 2.0},
    }
    assert read["read"]["event"] == {
        "uuid": event.uuid,
        "start": "2025-01-31 10:00:00+00:00",
        "stop": "2025-01-31 10:01:00+00:00",
        "author": "rust@x.org",
        "rating": 3,
        "tags": ["t1"],
        "products": ["p1"],
        "attributes": {"q": 1.5, "flag": True},
    }

    wait_until(lambda: any(seen.name == "beta" for seen in room.catalogues()))
    beta = room.catalogue("beta")
    assert beta.author == "python@x.org"
    assert beta.tags == {"p1", "p2"}
    assert beta.attributes == {"mission": "MMS", "level": 3}

    rust_event = room.event(read["written"]["event"])
    assert rust_event.start == datetime.datetime(
        2025, 2, 1, 9, 0, 0, 123456, tzinfo=UTC
    )
    assert rust_event.stop == datetime.datetime(2025, 2, 1, 9, 1, tzinfo=UTC)
    assert rust_event.author == "python@x.org"
    assert rust_event.rating == 5
    assert rust_event.tags == {"p3"}
    assert rust_event.products == {"p4"}
    assert rust_event.attributes == {"q": 0.25, "flag": False}
