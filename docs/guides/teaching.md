# Teaching with cocat

Finding events in real data is one of the best ways to learn space physics. Students look at
a week of solar wind and ask: is this a shock? Where does it start? cocat gives each student a
place to answer, and gives you a way to see and compare all the answers.

## Before the class

### Make a group for the class

Open the project menu → **Groups…** → create `Space plasma M2 – 2026`. Press **Share link**
in the group and send the link to the students, on the course page or by email. Each
student creates an account and joins the group.

From now on, giving the class access to a project is one click.

### Prepare the material

Make a project for the course, like `M2 – solar wind lab`, and put in it:

- a reference catalogue, if you have one: the events you expect them to find;
- a few example events, well described, so they see what a good event looks like;
- the instructions, as a README or a notebook in the project's **Files**.

Share it with the class group as **Viewer**. Students see everything, change nothing.

Community catalogues are good material too: the DONKI interplanetary shocks or the HELIO4CAST
ICMEs make a ready-made answer key. See [Community catalogues](public-catalogues.md).

## Three ways to run an exercise

### A. Everyone in one shared project

Make an exercise project and give the class group the **Editor** role. Each student makes a
catalogue with their name and adds events to it.

Good for: small groups, discussion in class. Everyone sees everyone's work live, which makes a
lively discussion on the projector. It also makes copying easy.

### B. Each student in their own project

Each student works in **My catalogues** or in a project they create, and shares it with you
as Viewer.

Good for: graded work. Nobody sees the others' answers.

### C. A labelling round

Use a [selection round](rounds.md) of kind **Labelling**, **blind**:

1. Each student is a proposer. The round makes them their own proposal to fill in.
2. In a blind round, students see only their own work. You, as decider or owner, see
   everyone's.
3. When the deadline passes, you see all the labels side by side. Interesting events are the
   ones the class disagreed on: bring them to the next lecture.

Good for: comparing how people label the same data, measuring agreement, and building a
consensus catalogue with the class.

## Follow the work

- **History** shows who added what, and when. No need to collect files.
- The round badges at the top show who is online. Helpful during a lab session.
- Smart catalogues help you grade: `author:student@uni.fr`, or
  `catalogue:"Alice" NOT catalogue:"Reference"` for what Alice found that the reference
  doesn't have.

## Python in class

Students using Jupyter install the package and sign in once (see
[Python and Jupyter](python.md)):

```python
import cocat_rs as cc
cc.login()
project = cc.open("M2 – solar wind lab")
```

The sign-in works on JupyterHub too: the link opens on their own laptop or phone.

A nice exercise: write a simple shock detector (a jump in |B| and in density), run it, write
its events to a catalogue, and compare with the reference by search or in pandas. Students
see the false positives and the misses for themselves.

## After the class

At the end of the year, export the catalogues if you want to keep them outside the server.
The project, its history and its rounds remain, so next year's class can start from what this
year's class built.
