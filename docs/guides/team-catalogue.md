# Build a catalogue with your team

Say three of you want a list of magnetopause crossings for a study. One person runs a
detection script. Two people check events by eye in SciQLop. A supervisor wants to follow
along. This guide shows how to set that up.

## 1. Make a project for the study

Open the project menu (the project name at the top) and choose **New project**. Call it after
the study, like `MP crossings – THEMIS 2008`.

Keep **My catalogues** for your personal notes. A study gets its own project, so you can share
it without sharing everything else.

## 2. Bring people in

Press **Share**.

- Your two colleagues: **Editor**.
- The supervisor: **Viewer**, if they only want to look. Or Editor, if they will fix things.

Send a **Share link** in your team chat, or an **Email invitation** to each person.

If you share many projects with the same people, make a **group** once (project menu →
**Groups…**) and give the group a role instead. New lab members then join one group, not ten
projects.

## 3. Agree on conventions

This is the step teams skip, and regret. Write down, before anyone adds events:

**What one event is.**
Does an event cover the whole crossing, or only the current sheet? Where exactly do start and
stop go? Two people with different habits make a catalogue nobody can use.

**Which tags you use.**
Keep a short list: `MP`, `multiple-crossing`, `boundary-layer`. The search box suggests the
tags already in use as you type, which helps people stay consistent.

**What status means.**
A common choice:

| Status | Meaning in this study |
|---|---|
| preliminary | found by the script, nobody looked yet |
| confirmed | one person checked the data |
| reviewed | a second person agreed |
| final | goes into the paper |
| rejected | looked like one, isn't |

Keep rejected events instead of deleting them. They make good negative examples, and they
stop the next person from adding the same false detection again.

**Which attributes, with units.**
For instance `spacecraft`, `normal_x`, `normal_y`, `normal_z`, `mlt`. cocat remembers each
attribute's type. Once events carry an attribute, give it a unit and a description from
Python:

```python
project.set_attribute_info("mlt", unit="h", description="magnetic local time of the crossing")
```

Put the conventions in the project itself, as a README file uploaded to the project's
**Files** (the **+** next to Files). Everyone who joins finds them there.

## 4. Split the work

Use several catalogues of the same project:

- `Script output` – what the detection code wrote (from [Python](python.md));
- `Checked by Ana`, `Checked by Ben` – each person's picks;
- `Final` – the agreed list.

An event can sit in several catalogues at once. Adding a script event to `Checked by Ana`
doesn't copy it. It is the same event, so when Ben fixes its stop time, it is fixed
everywhere.

Smart catalogues save a lot of clicking. A few useful ones (type the search, then **Save as
new smart catalogue…**):

| Name | Search |
|---|---|
| To check | `status:preliminary` |
| Disputed | `status:confirmed NOT catalogue:"Checked by Ben"` |
| Mine this week | `author:ana created:>now-7d` |
| Low confidence | `confidence:<0.5` |

They update themselves as people work.

## 5. Work at the same time

Everyone can edit at once, in the browser, SciQLop or Python. Nothing locks.

- The round badges at the top show who is here.
- A row someone is editing shows in their colour.
- Edits made offline, in SciQLop on a laptop on a train, merge when it reconnects.

## 6. Follow what changed

**History** lists every version: who changed what, and when. Pick one to see its added,
changed and removed events. Opened from a catalogue, the history shows only that catalogue.

When you reach a milestone ("all of 2008 checked"), press **Save version** and give it a name.
That is your checkpoint. You can always compare with it, or go back to it.

## 7. Look at the result

With no event selected, the panel on the right sums up the events in the table: how many,
the time they cover, durations, and the gaps between them. Pick a period on the timeline to
see the statistics of that period only.

When the list is done, see [Catalogues for a paper](paper.md) to freeze it and publish it.
