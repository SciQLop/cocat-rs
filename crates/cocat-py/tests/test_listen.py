import asyncio
import logging
import threading
import time

import pytest

pytestmark = pytest.mark.asyncio


class Recorder:
    def __init__(self):
        self.calls = []
        self.threads = set()
        self.batches = 0

    def _record(self, *call):
        self.threads.add(threading.get_ident())
        self.calls.append(call)

    def catalogue_added(self, uuid):
        self._record("catalogue_added", uuid)

    def catalogue_removed(self, uuid):
        self._record("catalogue_removed", uuid)

    def catalogue_renamed(self, uuid, name):
        self._record("catalogue_renamed", uuid, name)

    def catalogue_attributes(self, uuid, set, removed):
        self._record("catalogue_attributes", uuid, set, removed)

    def events_added(self, catalogue, uuids):
        self._record("events_added", catalogue, uuids)

    def events_removed(self, catalogue, uuids):
        self._record("events_removed", catalogue, uuids)

    def event_range(self, uuid, start, stop):
        self._record("event_range", uuid, start, stop)

    def event_attributes(self, uuid, set, removed):
        self._record("event_attributes", uuid, set, removed)

    def event_removed(self, uuid):
        self._record("event_removed", uuid)

    def status(self, value):
        self._record("status", value)

    def changes_done(self):
        self.batches += 1
        self._record("changes_done")


async def until(predicate, timeout=15.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        await asyncio.sleep(0.01)
    raise AssertionError("the listener did not see the change in time")


async def test_one_transaction_is_one_merged_batch(room, other):
    recorder = Recorder()
    handle = room.listen(recorder)
    try:
        with other.transaction():
            catalogue = other.create_catalogue(name="alpha", author="a@x.org")
            events = [
                other.create_event(
                    start=f"2025-01-31 10:0{index}:00",
                    stop=f"2025-01-31 10:0{index}:30",
                    author="a@x.org",
                )
                for index in range(3)
            ]
            catalogue.add_events(events)

        await until(lambda: recorder.batches == 1)
    finally:
        handle.close()

    assert recorder.calls[0] == ("status", "synced")
    assert recorder.calls[1] == ("catalogue_added", catalogue.uuid)
    assert recorder.calls[2][0] == "events_added"
    assert recorder.calls[2][1] == catalogue.uuid
    assert recorder.calls[2][2] == sorted(event.uuid for event in events)
    assert recorder.calls[-1] == ("changes_done",)


async def test_a_listener_hears_the_status_it_registers_under(room, other):
    recorder = Recorder()
    handle = room.listen(recorder)
    try:
        await until(lambda: recorder.calls)
        assert recorder.calls[0] == ("status", "synced")

        with other.transaction():
            other.create_catalogue(name="alpha", author="a@x.org")
        await until(lambda: recorder.batches == 1)

        assert recorder.calls[1][0] == "catalogue_added"
        assert [call for call in recorder.calls if call[0] == "status"] == [
            ("status", "synced")
        ]
    finally:
        handle.close()


async def test_an_unrelated_listener_hears_no_opening_status(room, other):
    class OnlyDone:
        batches = 0

        def changes_done(self):
            OnlyDone.batches += 1

    handle = room.listen(OnlyDone())
    try:
        other.create_catalogue(name="alpha", author="a@x.org")
        await until(lambda: OnlyDone.batches == 1)
    finally:
        handle.close()

    assert OnlyDone.batches == 1


async def test_the_listener_runs_on_the_loop_thread(room, other):
    recorder = Recorder()
    handle = room.listen(recorder)
    try:
        other.create_catalogue(name="alpha", author="a@x.org")
        await until(lambda: recorder.batches == 1)
    finally:
        handle.close()

    assert recorder.threads == {threading.get_ident()}


async def test_missing_methods_are_skipped(room, other):
    class OnlyDone:
        batches = 0

        def changes_done(self):
            OnlyDone.batches += 1

    handle = room.listen(OnlyDone())
    try:
        other.create_catalogue(name="alpha", author="a@x.org")
        await until(lambda: OnlyDone.batches == 1)
    finally:
        handle.close()


async def test_a_raising_method_is_logged_and_the_batch_goes_on(room, other, caplog):
    class Broken:
        def __init__(self):
            self.batches = 0
            self.names = []

        def catalogue_added(self, uuid):
            raise ValueError("this listener is broken")

        def catalogue_renamed(self, uuid, name):
            self.names.append(name)

        def changes_done(self):
            self.batches += 1

    listener = Broken()
    caplog.set_level(logging.ERROR, logger="cocat_rs")
    handle = room.listen(listener)
    try:
        catalogue = other.create_catalogue(name="alpha", author="a@x.org")
        await until(lambda: listener.batches == 1)

        catalogue.name = "beta"
        await until(lambda: listener.batches == 2)

        assert listener.names == ["beta"]
        assert "this listener is broken" in caplog.text
        assert "Traceback" in caplog.text

        other.create_catalogue(name="gamma", author="a@x.org")
        await until(lambda: listener.batches == 3)
    finally:
        handle.close()


async def test_a_local_write_is_not_reported_to_its_own_listener(room):
    recorder = Recorder()
    handle = room.listen(recorder)
    try:
        room.create_catalogue(name="alpha", author="a@x.org")
        await asyncio.sleep(0.5)
    finally:
        handle.close()

    assert recorder.calls == [("status", "synced")]


async def test_closing_the_handle_stops_the_calls(room, other):
    recorder = Recorder()
    handle = room.listen(recorder)
    other.create_catalogue(name="alpha", author="a@x.org")
    await until(lambda: recorder.batches == 1)

    handle.close()
    other.create_catalogue(name="beta", author="a@x.org")
    await asyncio.sleep(0.5)
    assert recorder.batches == 1
