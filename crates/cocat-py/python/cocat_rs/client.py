"""Signing in once and working without urls or cookies in the script.

`Client` holds a server and the session to it. `login()` signs in through the browser and
keeps the token in the system keyring, so the next Python session is signed in already.
The module functions of `cocat_rs` (`login`, `open`, `projects`...) use one default client.
"""

from __future__ import annotations

import os
import re
import socket
import sys
import threading
import warnings
import webbrowser
from collections.abc import Callable
from pathlib import Path
from typing import Any, ClassVar, TypeVar
from urllib.parse import urlparse

import keyring
import keyring.errors
import platformdirs

from . import Project, _native
from ._native import Forbidden, ProjectInfo, Unauthorized

DEFAULT_SERVER = "https://sciqlop.lpp.polytechnique.fr/cocat-dev"
KEYRING_SERVICE = "cocat_rs"
OPEN_TIMEOUT = 30.0

T = TypeVar("T")

# Tokens kept for this Python session only, by server, when the system has no keyring.
_MEMORY: dict[str, str] = {}
_DEFAULT: Client | None = None


class NotSignedIn(Unauthorized):
    """There is no sign-in for this server, or it ended: run `cocat_rs.login()`."""

    def __init__(self, message: str) -> None:
        super().__init__(message)
        self.status = 401
        self.detail = message


class SignIn:
    """A browser sign-in under way: show `url` and `code`, then `wait()`.

    `wait()` blocks, so an application runs it in a thread and calls `cancel()` to stop it.
    """

    _ENDINGS: ClassVar[dict[str, str]] = {
        "denied": "The sign-in was denied in the browser.",
        "expired": "The sign-in link expired: run cocat_rs.login() again.",
    }

    def __init__(self, client: Client, request: dict[str, Any]) -> None:
        self.url: str = request["url"]
        self.code: str = request["code"]
        self.interval: float = request["interval"]
        self.expires_in: float = request["expires_in"]
        self._poll_id: str = request["poll"]
        self._client = client
        self._cancelled = threading.Event()

    def poll(self) -> str:
        """`pending`, `approved`, `denied` or `expired`; once approved, the client is signed in."""
        answer = _native.poll_sign_in(self._client.server, self._poll_id)
        if answer["status"] == "approved":
            self._client._signed_in(answer["token"])
        return answer["status"]

    def wait(self) -> None:
        """Polls until the person answers; raises `NotSignedIn` unless they allowed it."""
        while not self._cancelled.wait(self.interval):
            status = self.poll()
            if status == "approved":
                return
            if status != "pending":
                raise NotSignedIn(self._ENDINGS[status])
        raise NotSignedIn("The sign-in was cancelled.")

    def cancel(self) -> None:
        self._cancelled.set()


class Client:
    """One server and the session to it."""

    def __init__(self, server: str | None = None) -> None:
        chosen = server or os.environ.get("COCAT_SERVER") or DEFAULT_SERVER
        self.server = chosen.rstrip("/")
        self._session: str | None = None
        self._credentials: tuple[str, str] | None = None
        self._account: str | None = None

    def __repr__(self) -> str:
        return f"Client({self.server!r})"

    @property
    def session(self) -> str | None:
        """The token (or cookie) this client sends, for the functions taking `(url, cookie)`."""
        if self._session is None:
            self._session = _stored_token(self.server)
        return self._session

    def login(self, email: str | None = None, password: str | None = None) -> None:
        """Signs in through the browser; with an email and a password, without it.

        The browser sign-in is remembered in the system keyring for the next sessions. A
        password sign-in lasts while Python runs, and nothing is stored.
        """
        if email is None and password is None:
            self._browser_login()
            return
        if not (email and password):
            raise ValueError(
                "Give both the email and the password, or neither to sign in in the browser."
            )
        self._session = _native.login(self.server, email, password)
        self._credentials = (email, password)
        self._account = None

    def start_sign_in(self, app: str = "Python") -> SignIn:
        """Starts a browser sign-in for `app`, shown as "<app> on <this machine>"."""
        request = _native.start_sign_in(self.server, f"{app} on {socket.gethostname()}")
        return SignIn(self, request)

    def logout(self) -> None:
        """Ends the session on the server and forgets it here."""
        session = self.session
        self._forget()
        if session is None:
            return
        try:
            _native.sign_out(self.server, session)
        except Unauthorized:
            pass  # already ended on the server

    def open(self, project_id: str | None = None, cache: str | Path | None = None) -> Project:
        """The project, by id or name, synced with the server; without one, your own project.

        Its copy is kept in `cache`, by default in this account's folder of the user cache.
        """
        project_id = self._project_id(project_id)
        project = Project.open(str(cache or self._cache_folder()), project_id)
        project._opened_with(self, project_id)
        project.connect(self.server, self._require_session(), self._credentials)
        try:
            project.wait_synced(OPEN_TIMEOUT)
        except RuntimeError:
            if project.status not in ("unauthorized", "no_access"):
                warnings.warn(
                    f"Could not reach {self.server}: working on the local copy, "
                    "it syncs when the server answers.",
                    stacklevel=2,
                )
                return project
            project.close()
            # A stale session raises here; otherwise the account is not in the project.
            self.account_id(fresh=True)
            raise _forbidden(
                f"You cannot open {project_id!r} on {self.server}: "
                "it does not exist or you are not a member."
            ) from None
        return project

    def account_id(self, fresh: bool = False) -> str:
        if self._account is None or fresh:
            self._account = self._call(_native.account_id)
        return self._account

    def account(self) -> dict[str, Any]:
        """The signed-in account: `id`, `email` and `is_curator`."""
        return self._call(_native.account)

    def projects(self) -> list[ProjectInfo]:
        return self._call(_native.projects)

    def create_project(self, name: str) -> ProjectInfo:
        return self._call(_native.create_project, name)

    def public_catalogues(self) -> list[dict[str, Any]]:
        return self._call(_native.public_catalogues)

    def copy_public_catalogue(
        self, public_id: str, project_id: str | None = None
    ) -> dict[str, str]:
        """Copies a public catalogue into one of your projects (a new one by default), to use it."""
        return self._call(_native.copy_public_catalogue, public_id, project_id)

    def propose_change(
        self, public_id: str, project_id: str | None = None, name: str | None = None
    ) -> dict[str, str]:
        return self._call(_native.propose_change, public_id, project_id, name)

    def create_public_catalogue(
        self,
        name: str,
        folder: str | None = None,
        source: str | None = None,
        description: str | None = None,
        key: str | None = None,
    ) -> str:
        return self._call(_native.create_public_catalogue, name, folder, source, description, key)

    def update_public_catalogue(
        self,
        id: str,
        name: str | None = None,
        folder: str | None = None,
        source: str | None = None,
        description: str | None = None,
    ) -> None:
        return self._call(_native.update_public_catalogue, id, name, folder, source, description)

    def submissions(self, mine: bool = False) -> list[dict[str, Any]]:
        return self._call(_native.submissions, mine)

    def submit_catalogue(
        self,
        project_id: str,
        catalogue_uuid: str,
        title: str | None = None,
        folder: str | None = None,
        description: str | None = None,
        note: str | None = None,
    ) -> int:
        return self._call(
            _native.submit_catalogue, project_id, catalogue_uuid, title, folder, description, note
        )

    def submit_update(self, project_id: str, catalogue_uuid: str, note: str | None = None) -> int:
        return self._call(_native.submit_update, project_id, catalogue_uuid, note)

    def review_submission(self, id: int, accept: bool, note: str | None = None) -> dict[str, Any]:
        return self._call(_native.review_submission, id, accept, note)

    def withdraw_submission(self, id: int) -> dict[str, Any]:
        return self._call(_native.withdraw_submission, id)

    def members(self, project_id: str) -> list[dict[str, str]]:
        return self._call(_native.members, project_id)

    def set_role(self, project_id: str, user_id: str, role: str) -> None:
        return self._call(_native.set_role, project_id, user_id, role)

    def remove_member(self, project_id: str, user_id: str) -> None:
        return self._call(_native.remove_member, project_id, user_id)

    def invite(
        self,
        project_id: str,
        role: str,
        email: str | None = None,
        expires_in_days: int = 14,
        max_uses: int | None = None,
    ) -> dict[str, Any]:
        return self._call(_native.invite, project_id, role, email, expires_in_days, max_uses)

    def invitations(self, project_id: str) -> list[dict[str, Any]]:
        return self._call(_native.invitations, project_id)

    def revoke_invitation(self, project_id: str, invitation_id: int) -> None:
        return self._call(_native.revoke_invitation, project_id, invitation_id)

    def project_groups(self, project_id: str) -> list[dict[str, str]]:
        return self._call(_native.project_groups, project_id)

    def set_editors_can_share(self, project_id: str, allowed: bool) -> None:
        return self._call(_native.set_editors_can_share, project_id, allowed)

    def project_url(self, project_id: str, share: bool = False) -> str:
        """The project's page in the web app; with `share`, its Share dialog."""
        return _native.project_url(self.server, project_id, share)

    def history(self, project_id: str, catalogue: str | None = None) -> list[dict[str, Any]]:
        return self._call(_native.history, project_id, catalogue)

    def version_changes(
        self,
        project_id: str,
        version: int,
        catalogue: str | None = None,
        since: int | None = None,
    ) -> dict[str, Any]:
        """What a version changed; with `since`, what changed between the two versions."""
        return self._call(_native.version_changes, project_id, version, catalogue, since)

    def commit(self, project_id: str, name: str | None = None, message: str | None = None) -> int:
        return self._call(_native.commit, project_id, name, message)

    def restore(self, project_id: str, version: int, catalogue: str | None = None) -> bool:
        return self._call(_native.restore, project_id, version, catalogue)

    def name_version(self, project_id: str, version: int, name: str | None) -> None:
        return self._call(_native.name_version, project_id, version, name)

    def copy_version(self, project_id: str, version: int, name: str) -> str:
        return self._call(_native.copy_version, project_id, version, name)

    def _call(self, function: Callable[..., T], *arguments: Any) -> T:
        """`function(server, session, *arguments)`; a refused session reads as `NotSignedIn`,
        except for a password sign-in, which signs in again once."""
        session = self._require_session()
        try:
            return function(self.server, session, *arguments)
        except Unauthorized:
            if self._credentials is None:
                self._forget()
                raise NotSignedIn(self._not_signed_in()) from None
        self.login(*self._credentials)
        return function(self.server, self._require_session(), *arguments)

    def _require_session(self) -> str:
        session = self.session
        if session is None:
            raise NotSignedIn(self._not_signed_in())
        return session

    def _not_signed_in(self) -> str:
        return f"Not signed in to {self.server}: run cocat_rs.login()"

    def _signed_in(self, token: str) -> None:
        self._session = token
        self._credentials = None
        self._account = None
        _store_token(self.server, token)

    def _forget(self) -> None:
        self._session = None
        self._credentials = None
        self._account = None
        _forget_token(self.server)

    def _browser_login(self) -> None:
        sign_in = self.start_sign_in()
        print(f"To sign in, open {sign_in.url} (code {sign_in.code})")
        _open_browser(sign_in.url)
        try:
            sign_in.wait()
        except KeyboardInterrupt:
            print("Sign-in cancelled.")
            raise
        print(f"Signed in to {self.server}.")

    def _cache_folder(self) -> Path:
        """One folder per server and account: a viewer's connect drops the changes the server
        never took, and must not reach another account's offline edits."""
        return (
            Path(platformdirs.user_cache_dir("cocat_rs"))
            / _folder_name(self.server)
            / self.account_id()
        )

    def _project_id(self, wanted: str | None) -> str:
        """The id of the project with this id or name; `None` is your own project."""
        projects = self.projects()
        if wanted is None:
            own = [project.id for project in projects if project.personal]
            if not own:
                raise ValueError(
                    f"You have no own project on {self.server}: give a project's id or name."
                )
            return own[0]
        if any(project.id == wanted for project in projects):
            return wanted
        named = [project.id for project in projects if project.name == wanted]
        if len(named) > 1:
            raise ValueError(
                f"Several of your projects are called {wanted!r}; open one by its id: {', '.join(named)}"
            )
        return named[0] if named else wanted


def default_client() -> Client:
    """The client the module functions use, made on first use."""
    global _DEFAULT
    if _DEFAULT is None:
        _DEFAULT = Client()
    return _DEFAULT


def use_server(server: str | None) -> Client:
    """Makes the default client one for `server` (or the default server)."""
    global _DEFAULT
    _DEFAULT = Client(server)
    return _DEFAULT


def _open_browser(url: str) -> None:
    """Opens the link when there is a screen to open it on; a headless Linux has none."""
    headless = sys.platform.startswith("linux") and not (
        os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY")
    )
    if headless:
        return
    try:
        webbrowser.open(url)
    except webbrowser.Error:
        pass  # the printed link is enough


def _folder_name(server: str) -> str:
    parsed = urlparse(server)
    return re.sub(r"[^A-Za-z0-9.-]+", "_", parsed.netloc + parsed.path).strip("_")


def _forbidden(message: str) -> Forbidden:
    error = Forbidden(message)
    error.status = 403
    error.detail = message
    return error


def _stored_token(server: str) -> str | None:
    if server in _MEMORY:
        return _MEMORY[server]
    try:
        return keyring.get_password(KEYRING_SERVICE, server)
    except keyring.errors.KeyringError:
        return None


def _store_token(server: str, token: str) -> None:
    try:
        keyring.set_password(KEYRING_SERVICE, server, token)
    except keyring.errors.KeyringError:
        _MEMORY[server] = token
        warnings.warn(
            "This system has no keyring: the sign-in is kept only until Python stops.",
            stacklevel=4,
        )


def _forget_token(server: str) -> None:
    _MEMORY.pop(server, None)
    try:
        keyring.delete_password(KEYRING_SERVICE, server)
    except keyring.errors.KeyringError:
        pass  # nothing stored, or no keyring at all
