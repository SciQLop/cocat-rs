"""Collaborative event catalogues, in Rust with a Python face."""

import asyncio
import logging
import threading

from . import _native, _old_names
from ._native import (
    Catalogue,
    Conflict,
    Event,
    Forbidden,
    NotFound,
    ProjectInfo,
    ServerError,
    ServerNotFound,
    Unauthorized,
    create_project,
    create_public_catalogue,
    invitations,
    invite,
    login,
    members,
    project_groups,
    project_url,
    projects,
    public_catalogues,
    remove_member,
    revoke_invitation,
    rule_from_tscat,
    set_editors_can_share,
    set_role,
)

_accepts_room_id = _old_names.renamed_keywords(room_id="project_id")
commit = _accepts_room_id(_native.commit)
copy_version = _accepts_room_id(_native.copy_version)
history = _accepts_room_id(_native.history)
name_version = _accepts_room_id(_native.name_version)
restore = _accepts_room_id(_native.restore)

__all__ = [
    "Catalogue",
    "Conflict",
    "Event",
    "Forbidden",
    "NotFound",
    "Project",
    "ProjectInfo",
    "ServerError",
    "ServerNotFound",
    "Unauthorized",
    "commit",
    "copy_version",
    "create_project",
    "create_public_catalogue",
    "history",
    "invitations",
    "invite",
    "login",
    "members",
    "name_version",
    "project_groups",
    "project_url",
    "projects",
    "public_catalogues",
    "remove_member",
    "restore",
    "revoke_invitation",
    "rule_from_tscat",
    "set_editors_can_share",
    "set_role",
]

log = logging.getLogger("cocat_rs")

_METHODS = (
    "catalogue_added",
    "catalogue_removed",
    "catalogue_renamed",
    "catalogue_attributes",
    "catalogue_rule",
    "events_added",
    "events_removed",
    "event_range",
    "event_attributes",
    "event_fields",
    "event_removed",
    "status",
    "changes_done",
)

_ARGUMENTS = {
    "catalogue_added": ("uuid",),
    "catalogue_removed": ("uuid",),
    "catalogue_renamed": ("uuid", "name"),
    "catalogue_attributes": ("uuid", "set", "removed"),
    "catalogue_rule": ("uuid",),
    "events_added": ("catalogue", "uuids"),
    "events_removed": ("catalogue", "uuids"),
    "event_range": ("uuid", "start", "stop"),
    "event_attributes": ("uuid", "set", "removed"),
    "event_fields": ("uuid", "set", "removed"),
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
    """What `Project.listen` returns; closing it stops the calls to that listener."""

    def __init__(self, project, registration):
        self._project = project
        self._registration = registration

    def close(self):
        self._project._remove(self._registration)


class _Transaction:
    def __init__(self, project):
        self._project = project

    def __enter__(self):
        self._project._native.begin_batch()
        return self._project

    def __exit__(self, kind, value, traceback):
        self._project._native.end_batch()
        return False


class Project:
    """One project, held on disk and synchronised in the background."""

    def __init__(self, native):
        self._native = native
        self._registrations = []
        self._lock = threading.Lock()
        self._thread = None
        self._running = False

    @classmethod
    @_old_names.renamed_keywords(room_id="project_id")
    def open(cls, cache_dir, project_id):
        return cls(_native.Project.open(str(cache_dir), str(project_id)))

    @classmethod
    @_old_names.renamed_keywords(room_id="project_id")
    def at(cls, url, cookie, project_id, version):
        """The project as of a version: read-only, and offline once fetched."""
        return cls(_native.Project.at(url, cookie, str(project_id), version))

    @property
    def status(self):
        return self._native.status

    def __getattr__(self, name):
        return getattr(self._native, name)

    def connect(
        self, url: str, cookie: str, credentials: tuple[str, str] | None = None
    ) -> None:
        self._native.connect(url, cookie, credentials)

    def set_cookie(self, cookie):
        self._native.set_cookie(cookie)

    def wait_synced(self, timeout=None):
        self._native.wait_synced(timeout)

    async def synced(self):
        loop = asyncio.get_running_loop()
        await loop.run_in_executor(None, self._native.wait_synced, None)

    def transaction(self):
        return _Transaction(self)

    def close(self):
        self._running = False
        self._native.close()

    def listen(self, listener):
        """Calls `listener` for every remote change, on the loop running right now."""
        loop = asyncio.get_running_loop()
        registration = _Registration(listener, loop)
        with self._lock:
            self._registrations.append(registration)
            if (status := registration.methods.get("status")) is not None:
                loop.call_soon_threadsafe(
                    self._call, registration, "status", status, (self._native.status,)
                )
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
        # Only later status changes are broadcast from here; listen() sends the current one.
        status = self._native.status
        while self._running:
            batch = self._native.next_batch(_POLL)
            current = self._native.status
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


def __getattr__(name):
    return _old_names.attribute(globals(), name)


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
