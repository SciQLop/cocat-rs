# Getting started

This page takes about ten minutes. You need a web browser and nothing else.

By the end you will have:

1. an account,
2. a catalogue with a few events in it,
3. a colleague looking at the same catalogue as you.

## 1. Create your account

Open the server address in your browser. Press **Create an account**.

Give your email and a password. The password needs at least 8 characters and must not be easy
to guess. A few unrelated words work well, like `plum orbit lantern cello`. If the page refuses
a password, it says why.

Some servers also let you sign in through your institution or ORCID. Then you don't need a
password at all.

If the server sends emails, you will get a link to confirm your address. Click it.

## 2. Your first project

When you sign in, you land in a project called **My catalogues**. It is yours, and nobody else
sees it until you share it.

A **project** is a box of catalogues. You share a whole project at once, so people often make
one project per collaboration or per study. You will make more later. For now, My catalogues
is fine.

## 3. Your first catalogue

On the left, press the **+** next to **Catalogues**, then **New catalogue…**.
Call it something like `Bow shock crossings`.

The catalogue is empty. That's normal.

## 4. Your first events

Press **New event**. An event has:

- a **start** and a **stop**, always in UTC;
- **tags**, short words to sort events, like `shock` or `quasi-parallel`;
- a **status**: preliminary, confirmed, reviewed, final or rejected;
- a **confidence**, from 0 to 1;
- **attributes**, your own named values, like `spacecraft = MMS1` or `density = 12.5`;
- a free-text **comment**.

Only start and stop are required. Fill in the rest when it helps you.

Add two or three events. They appear in the table and on the timeline above it.

!!! tip "Drag on the timeline"
    Press and drag on the timeline strip to pick a period. The table then shows only the
    events in that period, and the statistics on the right follow.

## 5. Share it

Press **Share** at the top. You have three ways to bring someone in:

- **Share link**: anyone with the link can join, with the role you pick. Paste it in a chat.
- **Email invitation**: a link sent to one address, which works once.
- **Group**: everyone in one of your groups gets the project at once.

Pick a **role** for them:

- **Viewer**: sees everything, changes nothing.
- **Editor**: adds, changes and deletes events and catalogues.
- **Owner**: that's you. There is one owner per project.

!!! warning "My catalogues is private on purpose"
    You can share My catalogues, but it is better to keep it for yourself. To work with
    others, open the project menu (the project name at the top) and choose **New project**.
    To bring a catalogue over, export it from My catalogues and import the file in the new
    project (see [Bring your catalogues in](guides/import-export.md)).

## 6. Work together

Once your colleague opens the project, you see a round badge at the top: that's them.

When they edit an event, the row lights up in their colour on your screen. Their changes reach
you within a second. Yours reach them the same way. **Live**, at the top right, tells you your
changes are getting to the server.

There is no "save" button for edits. Everything is kept as you go.

## 7. Go back in time

Press **History**. You see every version of the project, newest first.
Pick one to see what it changed. **Restore this version** brings the project back to that
point. The restore is itself a new version, so you can undo it too.

## Where next?

- Using Python or Jupyter? Read [Python and Jupyter](guides/python.md).
- Already have catalogues in tscat, AMDA or a VOTable? Read
  [Bring your catalogues in](guides/import-export.md).
- Want the big picture first? Read [Key ideas](concepts.md).
