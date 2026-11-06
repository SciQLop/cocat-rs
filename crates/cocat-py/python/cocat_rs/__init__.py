"""Collaborative event catalogues, in Rust with a Python face."""

import asyncio
import logging
import threading

from . import _native
from ._native import Catalogue, Event, RoomInfo, login, rooms

__all__ = [
    "Catalogue",
    "Event",
    "Room",
    "RoomInfo",
    "login",
    "rooms",
]

log = logging.getLogger("cocat_rs")

_METHODS = (
    "catalogue_added",
    "catalogue_removed",
    "catalogue_renamed",
    "catalogue_attributes",
    "events_added",
    "events_removed",
    "event_range",
    "event_attributes",
    "event_removed",
    "status",
    "changes_done",
)

_ARGUMENTS = {
    "catalogue_added": ("uuid",),
    "catalogue_removed": ("uuid",),
    "catalogue_renamed": ("uuid", "name"),
    "catalogue_attributes": ("uuid", "set", "removed"),
    "events_added": ("catalogue", "uuids"),
    "events_removed": ("catalogue", "uuids"),
    "event_range": ("uuid", "start", "stop"),
    "event_attributes": ("uuid", "set", "removed"),
    "event_removed": ("uuid",),
}

_POLL = 0.1


class _Registration:
    """One listener and the methods it actually has, resolved once here."""

    def __init__(self, listener, loop):
        self.listener = listener
        self.loop = loop
        self.methods = {
            name: method
            for name in _METHODS
            if callable(method := getattr(listener, name, None))
        }


class ListenHandle:
    """What `Room.listen` returns; closing it stops the calls to that listener."""

    def __init__(self, room, registration):
        self._room = room
        self._registration = registration

    def close(self):
        self._room._remove(self._registration)


class _Transaction:
    def __init__(self, room):
        self._room = room

    def __enter__(self):
        self._room._room.begin_batch()
        return self._room

    def __exit__(self, kind, value, traceback):
        self._room._room.end_batch()
        return False


class Room:
    """One room, held on disk and synchronised in the background."""

    def __init__(self, native):
        self._room = native
        self._registrations = []
        self._lock = threading.Lock()
        self._thread = None
        self._running = False

    @classmethod
    def open(cls, cache_dir, room_id):
        return cls(_native.Room.open(str(cache_dir), str(room_id)))

    @property
    def status(self):
        return self._room.status

    def __getattr__(self, name):
        return getattr(self._room, name)

    def connect(self, url, cookie):
        self._room.connect(url, cookie)

    def set_cookie(self, cookie):
        self._room.set_cookie(cookie)

    def wait_synced(self, timeout=None):
        self._room.wait_synced(timeout)

    async def synced(self):
        loop = asyncio.get_running_loop()
        await loop.run_in_executor(None, self._room.wait_synced, None)

    def transaction(self):
        return _Transaction(self)

    def close(self):
        self._running = False
        self._room.close()

    def listen(self, listener):
        """Calls `listener` for every remote change, on the loop running right now."""
        loop = asyncio.get_running_loop()
        registration = _Registration(listener, loop)
        with self._lock:
            self._registrations.append(registration)
            if self._thread is None:
                self._running = True
                self._thread = threading.Thread(
                    target=self._dispatch, name="cocat_rs-listener", daemon=True
                )
                self._thread.start()
        return ListenHandle(self, registration)

    def _remove(self, registration):
        with self._lock:
            if registration in self._registrations:
                self._registrations.remove(registration)

    def _dispatch(self):
        # A listener hears status *changes*; the status it registers under is not news.
        status = self._room.status
        while self._running:
            batch = self._room.next_batch(_POLL)
            current = self._room.status
            if current != status:
                status = current
                self._broadcast("status", (current,))
            if not batch:
                continue
            for change in batch:
                kind = change.pop("kind")
                self._broadcast(kind, tuple(change[name] for name in _ARGUMENTS[kind]))
            self._broadcast("changes_done", ())

    def _broadcast(self, method, arguments):
        with self._lock:
            registrations = list(self._registrations)
        for registration in registrations:
            function = registration.methods.get(method)
            if function is None:
                continue
            registration.loop.call_soon_threadsafe(
                self._call, registration, method, function, arguments
            )

    def _call(self, registration, method, function, arguments):
        try:
            function(*arguments)
        except Exception:
            log.exception(
                "%s() of %r raised; the rest of the batch still runs",
                method,
                registration.listener,
            )


# The script mode re-exports Python cocat's names, so `import cocat_rs as cocat`
# gives a script the same surface.
from . import api  # noqa: E402
from .api import (  # noqa: E402
    create_catalogue,
    create_event,
    export_votable,
    get_catalogue,
    get_event,
    import_votable,
    log_in,
    log_out,
    refresh,
    save,
    set_config,
)

__all__ += [
    "api",
    "create_catalogue",
    "create_event",
    "export_votable",
    "get_catalogue",
    "get_event",
    "import_votable",
    "log_in",
    "log_out",
    "refresh",
    "save",
    "set_config",
]
