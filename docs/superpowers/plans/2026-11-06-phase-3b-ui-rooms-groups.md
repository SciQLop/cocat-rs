# Phase 3b — Web UI: rooms, invitations, groups — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The rest of the web UI: my rooms, one room (members, invitations, groups),
joining a room by invitation, and the new group pages.

**Architecture:** Same frame as 3a: askama pages, one plain form POST per action, answered
by a 303 back to the page (post/redirect/get). The rules already exist (`lifecycle`,
`invitations`, `groups`); the UI only calls them and turns their errors into messages.
Messages that name something ("Removed bob@x.org", a new share link) travel in a **flash
cookie** set on the 303 and cleared by the next GET, read and written with the `cookie`
crate.

**Tech Stack:** existing. Two more tiny static scripts, loaded by the frame: `confirm.js`
(a `data-confirm` attribute on a form asks `confirm()` before submitting) and `copy.js`
(a `data-copy` button copies a field with `navigator.clipboard`). No inline scripts.

**Spec:** design phase 3, plus phase 2d's groups. Behaviour reference: the Python test
checkout `../cocat-python-tests`, `src/cocat/ui/{rooms,room,
invitation,token}.py` and `tests/test_ui_{rooms,room,invitation}.py`. Read them before each
task. The group pages have no Python counterpart: they mirror the room pages.

## Global Constraints

- This repo, branch `main`, commit per task, never push.
  Rules file (including "use established crates"), TDD, one build/test command at a time in
  the foreground, full gate after the last edit of each task:
  `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --workspace`.
- Every page needs a user (`RequireUser`); every POST goes through 3a's same-site layer.
- **Flash cookie:** name `cocat_flash`, `Path={prefix}/ui`, `HttpOnly`, `Secure`,
  `SameSite=Lax`, `Max-Age=60`, value = the message text (the `cookie` crate encodes it).
  Shown escaped like any user text; cleared on the GET that shows it. One helper pair in
  `ui/frame.rs`: `flash(response, text)` and the `Flash` extractor.
- Messages are Python's, verbatim, including the interpolated ones. Errors from the rule
  modules are shown with their `detail` text, as Python shows `str(error)`.
- A room or group the user can't see answers the same page as Python's "Room not found"
  (status 404 here, with a "Back to my rooms" link), whether it doesn't exist or they aren't
  a member. A POST on it does nothing and redirects to the list with the error flashed.
- Tokens (share links, invitation tokens) never appear in a URL the server receives: they
  travel in `#fragments` and POST bodies only. The share link shown to an owner is
  `links.ui("/invitation#<token>")`, as today.
- Names: stripped; empty → "Enter a room name" / "Enter a group name"; any control
  character → "Enter a room name without control characters" (same for groups); over 200
  characters → "Enter a name of at most 200 characters".

## Review Focus

1. **Acting on someone else's object by id:** revoke an invitation id that belongs to
   another room, remove a member of a room you don't own, grant a room to a group you're
   not in, by crafting the POST. Expect: refused by the rule module, no side effect.
   → tests in Tasks 2, 3, 4.
2. **A forged `cocat_flash` cookie** containing `<script>`: shown as text. → Task 1 test.
3. **Access lost between page load and click** (removed from the room, session
   deactivated): the POST is refused with a message, nothing changes. → Task 2 test.
4. **Invitation tokens never in a query string:** the preview and accept forms are POSTs
   with the token in the body; the anonymous "Log in to accept" link carries no token.
   → Task 3 test.
5. **Group picker lists only the owner's groups**, and a grant POST with another group's
   id is refused (404 from `groups::grant`). → Task 3 test.

---

### Task 1: Flash, scripts, my rooms

**Files:** `ui/frame.rs` (flash), `ui/rooms.rs` (replaces the 3a placeholder),
`templates/rooms.html`, `static/{confirm.js,copy.js}`, `templates/base.html` (header adds
"My groups" → `/ui/groups`; the groups page itself comes in Task 4, a placeholder until then).
Test: `tests/ui_rooms.rs`.

- `GET /ui/rooms`: Python's page: create form ("New room name", "Create"), the user's
  rooms through `db::list_user_rooms` (direct and through groups) with name link, role, and
  owner: Rename (inline name field) and Delete (`data-confirm="Delete {name}?"`);
  non-owner: Leave (`data-confirm="Leave {name}?"`).
- `POST /ui/rooms` (create, flash "Room created"), `POST /ui/rooms/{id}/rename` ("Room
  renamed"), `POST /ui/rooms/{id}/delete` ("Room deleted"), `POST /ui/rooms/{id}/leave`
  ("You left the room"). All 303 → `/ui/rooms`.
- [ ] Tests (from `test_ui_rooms.py`): `login_required`, `lists_rooms_with_roles`,
  `lists_rooms_reached_through_a_group`, `create`, `create_requires_a_name`,
  `create_refuses_control_characters`, `rename`, `delete`, `leave`, `owner_has_no_leave`,
  `action_refused_after_deactivation`, `flash_shown_once`, `forged_flash_is_escaped`
  (review focus 2), and add every new POST path to 3a's `cross_site_posts_are_refused` table.
- [ ] Red, implement, gate. Commit "My rooms in the web UI".

### Task 2: One room: members and invitations

**Files:** `ui/room.rs`, `templates/room.html`. Test: `tests/ui_room.rs`.

- `GET /ui/rooms/{id}`: heading, "My rooms" link, "Your role: {role}", Leave for
  non-owners; "Members" ordered by email (direct members; group access is in Task 3's
  section). For the owner, per non-owner member: a role select (`POST
  /ui/rooms/{id}/members/{user}/role`, flash "{email} is now a {role}"), Remove (`.../remove`,
  confirm "Remove {email}?", flash "Removed {email}"), Transfer ownership (`.../owner`,
  confirm "Make {email} the owner?", flash "{email} now owns the room").
- Invitations (owner only; others see "Only the owner can manage invitations."): role select
  [editor, viewer]; "Create share link" (`POST /ui/rooms/{id}/invitations` without email:
  unlimited uses, 14 days; flash carries the link, shown in the read-only "Share link"
  field with a Copy button); "Invite by email" (same route with `email`: mailed when a
  mailer exists, flash "Invitation sent to {address}"; without a mailer the link is shown
  as for a share link, flash "Invitation created"; a failed mail → Python's message); the
  list of usable invitations, newest first (email or "Share link", role, expiry date,
  "uses/max" when limited, Revoke with confirm "Revoke this invitation?" → `POST
  /ui/rooms/{id}/invitations/{inv}/revoke`, flash "Invitation revoked").
- [ ] Tests (from `test_ui_room.py`): `login_required`, `not_found_for_strangers` (404,
  same page for missing and foreign), `owner_view`, `editor_view_has_no_role_picker`,
  `viewer_view`, `change_role_closes_their_socket`, `remove`, `transfer`, `leave`,
  `share_link_uses_public_url`, `revoke`, `invite_by_email_with_mailer`,
  `invite_by_email_without_mailer_shows_link`, `refused_after_losing_access` (review focus
  3), `revoke_foreign_invitation_is_refused` (review focus 1: invitation of room B revoked
  through room A's URL, where the actor owns A only).
- [ ] Red, implement, gate. Commit "Room page: members and invitations".

### Task 3: Room groups and joining by invitation

**Files:** `ui/room.rs`, `templates/room.html`, `ui/invitation.rs`,
`templates/invitation.html`. Tests: `tests/ui_room.rs`, `tests/ui_invitation.rs`.

- Room page section "Groups": every grant (group name, role) for any member. The owner
  also gets: a select of *their own* groups not yet granted plus a role select, "Share with
  group" (`POST /ui/rooms/{id}/groups`, flash "Shared with {group}"); per grant a role select
  (same route, upsert, flash "{group} is now {role}") and Remove (`POST
  /ui/rooms/{id}/groups/{gid}/remove`, confirm "Stop sharing with {group}?", flash
  "Stopped sharing with {group}").
- `GET /ui/invitation`: Python's page. The `#token` is moved by `fragment.js` into a hidden
  field of a form that POSTs to `/ui/invitation` (auto-submitted once, like verify); without
  a token, the "Invitation link" paste box (`token_from_text` rule: text after `#` if any,
  first whitespace-separated word). `POST /ui/invitation` renders the preview (200): "{inviter}
  invites you to {room}", "You join as {role}", "The invitation lasts until YYYY-MM-DD"
  (inviter unknown → "Someone", not Python's "None"), or "This invitation is not valid
  anymore". Signed in: an Accept form (token in a hidden field) → `POST
  /ui/invitation/accept` → 303 `/ui/rooms/{id}`, flash "You joined {room}". Anonymous: "Log
  in to accept" → `/ui/login?next=/ui/invitation` and "Signing in leaves this page behind:
  paste the link again."
- [ ] Tests: room groups — `grants_listed_for_viewers`, `owner_picker_lists_only_own_groups`,
  `share_with_group`, `change_group_role`, `stop_sharing`, `grant_foreign_group_refused`
  (review focus 5); invitation (from `test_ui_invitation.py`) — `preview`, `accept`,
  `second_accept_when_exhausted`, `wrong_email`, `invalid_token`, `paste_a_link`,
  `paste_a_bare_token`, `logged_out_link_carries_no_token` (review focus 4),
  `token_only_in_post_bodies` (review focus 4: the forms are POST and no `action` contains
  the token).
- [ ] Red, implement, gate. Commit "Room groups and invitation page".

### Task 4: Group pages

**Files:** `ui/groups.rs`, `templates/{groups,group,group_join}.html`. Test: `tests/ui_groups.rs`.

- `GET /ui/groups`: create ("New group name", "Create", flash "Group created"); the user's
  groups with role; owner: Rename, Delete (confirm "Delete {name}? Its rooms stop being
  shared with its members."); member: Leave (confirm "Leave {name}?").
- `GET /ui/groups/{id}`: name, "My groups" link, "Your role", members by email; owner:
  Remove per member (confirm, flash "Removed {email}"); invitations exactly like a room's
  but without a role select (share link, invite by email, list, revoke); "Rooms shared with
  this group": name (link) and role.
- `GET /ui/groups/join` (`#token` → `POST /ui/groups/join` preview: "{inviter} invites you
  to the group {name}"; Accept → `POST /ui/groups/join/accept` → 303 `/ui/groups/{id}`,
  flash "You joined {name}"). Links in group invitation mails already point here.
- Non-members: the "Group not found" page (404).
- [ ] Tests: `login_required`, `create_list_rename_delete`, `leave`, `owner_cannot_leave`,
  `members_and_remove`, `remove_closes_sockets_in_shared_rooms`, `share_link_and_join`,
  `invite_by_email`, `revoke`, `not_found_for_strangers`, `remove_from_foreign_group_refused`
  (review focus 1), `rooms_shared_with_group_listed`.
- [ ] Red, implement, gate. Commit "Group pages in the web UI".

### Task 5: Docs

- [ ] README "Web UI": add the room, invitation and group pages to the table, and one line
  on the flash cookie. Spec status line: phase 3a–3b done. Commit "Document the room and
  group pages".
