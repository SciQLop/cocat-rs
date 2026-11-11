# Questions and answers

## Accounts

**I forgot my password.**
Use **Forgot your password?** on the sign-in page. You get a link by email.

**My password is refused.**
It needs at least 8 characters and must not be easy to guess. The page says what is wrong. A
few unrelated words work well: `plum orbit lantern cello`.

**I sign in through my institution. Where do I change my password?**
At your institution. cocat never sees it.

**I lost my laptop.**
Open **Account** in the browser. Under **Signed-in apps**, revoke the one on that laptop. It
stops working at once.

## Projects and sharing

**Who can see my catalogues?**
Only the people in the project. **My catalogues** is private until you share it. Public
catalogues are the only ones every account can read.

**I want to share some catalogues, not all.**
Make a new project, put those catalogues in it, and share that project.

**Someone left the lab. How do I remove them?**
**Share** → their row → remove. If they had access through a group, remove them from the
group too.

**Can I give the project to someone else?**
Yes, if you are the owner: **Share** → **Transfer ownership…**. You stay as an editor.

**What happens if I delete a project?**
Its catalogues and history are deleted for everyone. This can't be undone. Before deleting,
consider making a copy of the last version, or exporting the catalogues.

## Editing

**Is there a save button?**
Not for edits: every change is kept as you make it. **Save version** is different: it names a
point in the history, so you can find it again.

**Two of us changed the same event. Whose change won?**
Changes to different fields are both kept. If you both changed the same field at the same
moment, one value is kept, the same for everyone. **History** shows who changed what.

**I deleted something by mistake.**
Look in the **Trash**, at the bottom of the catalogue list, and press **Restore**. If you
emptied the trash, restore an earlier version from **History**.

**Why are times in UTC?**
Space physics data is in UTC, and so are spacecraft clocks once converted. One time scale for
everyone avoids mistakes when people work from different countries.

**Can an event be in two catalogues?**
Yes, within the same project. It's the same event, so a fix shows in both.

## Python

**`cc.login()` printed a link. What do I do?**
Open it in any browser, on any device. Sign in, check the code matches, press **Allow**.

**`NotSignedIn` after it worked yesterday.**
The sign-in was revoked, or was not used for six months. Run `cc.login()` again.

**I'm a viewer and my script fails with "read-only".**
Viewers can't change a project. Ask the owner for the editor role, or make a copy of the
project's version with `project.copy_version(...)` and work on that.

**My script ends before the events reach the server.**
Call `project.save()` before the end. It waits until the server has everything.

**Does it work offline?**
Yes. Your copy is kept on disk. Changes go out when the server answers again.

## Limits

**How big can a catalogue be?**
Hundreds of thousands of events work. The STIX flare list, about 100 000 events, opens in a
few seconds. Very large catalogues load the first rows first, then the rest.

**Is there a storage limit?**
Each server sets one, per project and per account. When a project is full, the server refuses
further changes and says why. Ask your server's administrator if you need more.
