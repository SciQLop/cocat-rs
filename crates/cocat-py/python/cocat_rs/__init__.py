"""Collaborative event catalogues, in Rust with a Python face."""

import asyncio
import logging
import threading

from . import _native, round_review
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
    commit,
    copy_public_catalogue,
    copy_version,
    create_project,
    create_public_catalogue,
    history,
    invitations,
    invite,
    login,
    members,
    name_version,
    parse_query,
    project_groups,
    project_url,
    projects,
    propose_change,
    public_catalogues,
    remove_member,
    restore,
    review_submission,
    revoke_invitation,
    round_cost as _round_cost,
    rule_from_tscat,
    set_editors_can_share,
    set_role,
    sign_out,
    submissions,
    submit_catalogue,
    submit_update,
    tfcat_json,
    update_public_catalogue,
    withdraw_submission,
)

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
    "copy_public_catalogue",
    "copy_version",
    "create_project",
    "create_public_catalogue",
    "history",
    "invitations",
    "invite",
    "login",
    "members",
    "name_version",
    "parse_query",
    "project_groups",
    "project_url",
    "projects",
    "propose_change",
    "public_catalogues",
    "remove_member",
    "restore",
    "review_submission",
    "revoke_invitation",
    "round_cost",
    "round_review",
    "rule_from_tscat",
    "set_editors_can_share",
    "set_role",
    "sign_out",
    "submissions",
    "submit_catalogue",
    "submit_update",
    "tfcat_json",
    "update_public_catalogue",
    "withdraw_submission",
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
    "role",
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
# What a listener hears as a value: once when it registers, then on every change.
_WATCHED = ("status", "role")


def _cost_row(event):
    if isinstance(event, dict):
        return event
    return {
        "uuid": event.uuid,
        "start": event.start,
        "stop": event.stop,
        "attributes": event.attributes,
    }


def round_cost(events, budget):
    """What `events` (events, or dicts with `uuid`, `start`, `stop`, `attributes`) cost
    under a round's `budget`: a dict with `total`, `cap`, `unit`, `unpriced` and `text`."""
    return _round_cost([_cost_row(event) for event in events], budget)


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
        self._client = None
        self._id = None

    def _opened_with(self, client, project_id):
        """Remembers the client `cocat_rs.open` used, for the calls below."""
        self._client = client
        self._id = project_id

    def _server_call(self, method, *arguments, **keywords):
        if self._client is None:
            raise RuntimeError(
                "This project was opened from a folder: open it with cocat_rs.open() "
                "to reach its history and submissions."
            )
        return getattr(self._client, method)(self._id, *arguments, **keywords)

    def open_link(self, link_id, cache=None):
        """Opens the project link `link_id` points into: answers it and the linked catalogue.
        Changes made there go to the original project. Close the project when done."""
        if self._client is None:
            raise RuntimeError("This project was opened from a folder: open it with cocat_rs.open().")
        link = next((link for link in self.links() if link["id"] == link_id), None)
        if link is None:
            raise LookupError(f"no link {link_id!r} in this project")
        original = self._client.open(link["project"], cache=cache)
        return original, original.catalogue(link["catalogue"])

    def history(self, catalogue=None):
        """The saved versions, newest first; with a catalogue's uuid, those that changed it."""
        return self._server_call("history", catalogue)

    def version_changes(self, version, catalogue=None, since=None):
        """What a version changed; with `since`, what changed between the two versions."""
        return self._server_call("version_changes", version, catalogue, since)

    def commit(self, name=None, message=None, timeout=None):
        """Sends every change made here, then saves them as a version; returns its id."""
        self.save(timeout)
        return self._server_call("commit", name, message)

    def restore(self, version, catalogue=None):
        """Makes the project, or one catalogue, as it was at the version again."""
        return self._server_call("restore", version, catalogue)

    def name_version(self, version, name):
        """Names a version, or takes its name away with `None`."""
        return self._server_call("name_version", version, name)

    def copy_version(self, version, name):
        """A new project of yours holding the version; returns its id."""
        return self._server_call("copy_version", version, name)

    def submit(self, catalogue, title=None, folder=None, description=None, note=None):
        """Offers a catalogue (or its uuid) as a new public catalogue; the submission's id."""
        uuid = getattr(catalogue, "uuid", catalogue)
        return self._server_call("submit_catalogue", uuid, title, folder, description, note)

    def submit_update(self, catalogue, note=None):
        """Offers a copy made with `propose_change` as the public catalogue's next version."""
        uuid = getattr(catalogue, "uuid", catalogue)
        return self._server_call("submit_update", uuid, note)

    def rounds(self):
        """The selection rounds of this project."""
        return self._server_call("rounds")

    def create_round(
        self,
        name,
        visibility="open",
        deadline=None,
        budget=None,
        kind=None,
        candidates=None,
        proposers=None,
        voters=None,
        deciders=None,
    ):
        """A selection round; `visibility` is `open` or `blind`, `kind` `review`, `downlink`
        or `labelling`. `candidates` is a catalogue you can read (or its `(project id,
        catalogue uuid)`), copied into the round for review. `proposers`, `voters` and
        `deciders` are member emails; a role left out keeps its default, and a round without
        proposers starts in voting. Returns the round as a dict."""
        return self._server_call(
            "create_round",
            name,
            visibility,
            deadline,
            budget,
            kind,
            candidates,
            proposers=proposers,
            voters=voters,
            deciders=deciders,
        )

    def round(self, round_id):
        return self._server_call("round", round_id)

    def update_round(self, round_id, **fields):
        """Changes `name`, `visibility`, `deadline` or `budget`; `None` clears the last two."""
        return self._server_call("update_round", round_id, **fields)

    def set_round_members(self, round_id, proposers, voters, deciders):
        """Replaces who proposes, votes and decides, by email."""
        return self._server_call("set_round_members", round_id, proposers, voters, deciders)

    def move_round(self, round_id, state, final_catalogue=None, note=None):
        """Moves the round to `state`, with a `note` shown in its log; deciding needs the
        final catalogue's uuid, see `decide`."""
        return self._server_call("move_round", round_id, state, final_catalogue, note)

    def decide(self, round_id, events, note=None):
        """Writes `events` (dicts, as `round_review.final_events` returns them) as the
        round's final catalogue, replacing the events it holds when the round was decided
        before (and bringing it back from the trash), then moves the round to decided. Deciding a
        decided round only rewrites the catalogue. This project must be the round's."""
        uuid = round_review.final_catalogue_uuid(round_id)
        name = self.round(round_id)["name"]
        catalogue = self._final_catalogue(uuid, name)
        self._replace_events(catalogue, events)
        self.save(15)
        if self.round(round_id)["state"] == "decided":
            return self.round(round_id)
        return self.move_round(round_id, "decided", uuid, note)

    def _final_catalogue(self, uuid, name):
        try:
            catalogue = self.catalogue(uuid)
        except LookupError:
            return self.create_catalogue(name, uuid=uuid)
        if catalogue.deleted:
            catalogue.restore()
        return catalogue

    def _replace_events(self, catalogue, events):
        for old in catalogue.events:
            old.purge()
        catalogue.add_events([self._create_final_event(event) for event in events])

    def _create_final_event(self, event):
        shape = event.get("shape")
        fields = {
            key: event.get(key)
            for key in (
                "uuid", "author", "created", "origin", "confidence", "start_err", "stop_err",
                "source_id", "tags", "products", "attributes", "status",
            )
        }
        return self.create_event(
            event["start"],
            event["stop"],
            shape=None if shape is None else [(v["time"], v["value"]) for v in shape],
            **{key: value for key, value in fields.items() if value is not None},
        )

    def my_proposal(self, round_id, cache=None):
        """Your proposal document of the round, a project of its own; close it when done."""
        return self._server_call("my_proposal", round_id, cache)

    def vote(self, round_id, events):
        """Replaces your votes with these events (or their uuids)."""
        uuids = [getattr(event, "uuid", event) for event in events]
        return self._server_call("vote", round_id, uuids)

    def votes(self, round_id):
        return self._server_call("votes", round_id)

    def verdict(self, round_id, event, value):
        """Your verdict on an event (or its uuid): `accept`, `reject`, `unsure`, or `None`."""
        return self._server_call("verdict", round_id, event, value)

    def set_verdicts(self, round_id, verdicts):
        """Merges your verdicts, `{event uuid: verdict or None}`."""
        return self._server_call("set_verdicts", round_id, verdicts)

    def verdicts(self, round_id):
        """`{revision, events: [{event, accept, reject, unsure, by?}]}`, as far as you may read."""
        return self._server_call("verdicts", round_id)

    @classmethod
    def open(cls, cache_dir, project_id):
        return cls(_native.Project.open(str(cache_dir), str(project_id)))

    @classmethod
    def at(cls, url, cookie, project_id, version):
        """The project as of a version: read-only, and offline once fetched."""
        return cls(_native.Project.at(url, cookie, str(project_id), version))

    @property
    def status(self):
        return self._native.status

    def __getattr__(self, name):
        return getattr(self._native, name)

    def connect(
        self,
        url: str,
        cookie: str | None = None,
        credentials: tuple[str, str] | None = None,
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

    def import_dataframe(self, frame, name):
        """A new catalogue named `name` from a pandas or polars DataFrame, a row per event.
        It needs `start` and `stop` columns; `uuid`, `tags`, `status`, `confidence`, `author`
        and `comment` are the event's fields, other columns its attributes. A row whose uuid
        the project holds updates that event: the cells it has replace those values."""
        from . import frames

        return frames.import_frame(self, frame, name)

    def merge(self, source):
        """Brings in another project's catalogues, events and registry, as one edit of this
        one; returns how many catalogues and events were new here."""
        return self._native.merge(source._native)

    def close(self):
        self._running = False
        self._native.close()

    def listen(self, listener):
        """Calls `listener` for every remote change, on the loop running right now."""
        loop = asyncio.get_running_loop()
        registration = _Registration(listener, loop)
        with self._lock:
            self._registrations.append(registration)
            for name in _WATCHED:
                if (method := registration.methods.get(name)) is not None:
                    loop.call_soon_threadsafe(
                        self._call, registration, name, method, (getattr(self._native, name),)
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
        # Only later changes are broadcast from here; listen() sends the current values.
        # Compared between polls: a reconnect can pass through "connecting" within one poll,
        # so a role change is announced by its own call, not by the status.
        last = {name: getattr(self._native, name) for name in _WATCHED}
        while self._running:
            batch = self._native.next_batch(_POLL)
            for name in _WATCHED:
                current = getattr(self._native, name)
                if current != last[name]:
                    last[name] = current
                    self._broadcast(name, (current,))
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
    export_csv,
    export_hpevent,
    export_tfcat,
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
    "export_csv",
    "export_hpevent",
    "export_tfcat",
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


# The short form: one default client behind module functions, `cc.login()` then `cc.open()`.
from . import client  # noqa: E402
from .client import (  # noqa: E402
    DEFAULT_SERVER,
    Client,
    NotSignedIn,
    SignIn,
    default_client,
)


def login(*arguments, server=None, email=None, password=None):
    """Signs in to `server` (default: `$COCAT_SERVER`, else the public server).

    - `login()` or `login(server)`: in the browser; remembered for the next sessions.
    - `login(email, password)`: without a browser, for this Python session.

    Applications that hold a session themselves use `cocat_rs.raw.login(url, email, password)`.
    """
    if len(arguments) == 1:
        (server,) = arguments
    elif len(arguments) == 2:
        email, password = arguments
    elif arguments:
        raise TypeError("login() takes a server, or an email and a password")
    chosen = client.use_server(server) if server is not None else default_client()
    chosen.login(email, password)


def logout():
    """Signs out of the default server and forgets the sign-in."""
    default_client().logout()


def open(project_id=None, cache=None):  # noqa: A001 - `cc.open` is the name scientists type
    """The project, by id or name, synced; without one, your own project."""
    return default_client().open(project_id, cache)


def start_sign_in(app="Python"):
    """A browser sign-in in two steps, for applications: show its link, then `wait()`."""
    return default_client().start_sign_in(app)


def _shortcut(name):
    """The module function `name`: the default client's method."""

    def shortcut(*arguments, **keywords):
        return getattr(default_client(), name)(*arguments, **keywords)

    shortcut.__name__ = shortcut.__qualname__ = name
    shortcut.__doc__ = getattr(Client, name).__doc__
    return shortcut


for _name in (
    "account",
    "account_id",
    "commit",
    "copy_public_catalogue",
    "copy_version",
    "create_project",
    "create_public_catalogue",
    "history",
    "invitations",
    "invite",
    "members",
    "name_version",
    "project_groups",
    "project_url",
    "projects",
    "propose_change",
    "public_catalogues",
    "remove_member",
    "restore",
    "review_submission",
    "revoke_invitation",
    "set_editors_can_share",
    "set_role",
    "submissions",
    "submit_catalogue",
    "submit_update",
    "update_public_catalogue",
    "withdraw_submission",
):
    globals()[_name] = _shortcut(_name)
del _name

from . import raw  # noqa: E402

# `open` stays out of `__all__`: a star import must not hide the builtin.
__all__ += [
    "DEFAULT_SERVER",
    "raw",
    "account",
    "Client",
    "NotSignedIn",
    "SignIn",
    "account_id",
    "client",
    "default_client",
    "logout",
    "start_sign_in",
]
