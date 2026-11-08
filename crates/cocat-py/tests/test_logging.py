import logging
import socket
import time

import cocat_rs as cc


def a_closed_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def test_a_connection_failure_reaches_python_logging(tmp_path, caplog):
    caplog.set_level(logging.WARNING)
    project = cc.Project.open(tmp_path, "lab")
    project.connect(f"http://127.0.0.1:{a_closed_port()}", "fastapiusersauth=x")

    deadline = time.monotonic() + 5
    while not caplog.records and time.monotonic() < deadline:
        time.sleep(0.05)
    project.close()

    assert any(
        record.name.startswith("cocat_client") and "cannot reach the room" in record.getMessage()
        for record in caplog.records
    ), [record.getMessage() for record in caplog.records]
