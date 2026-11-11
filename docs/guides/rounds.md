# Review rounds

Sometimes several people must choose events together, and you need to know who said what.

- Your detection pipeline found 600 candidate flux ropes. Three people should check them.
- The mission team can download 2 Gbit of burst data this week. Each scientist proposes
  intervals, and a decider keeps what fits.
- A class labels the same week of data, and you want to compare their answers.

A **selection round** does this. It ends with one **final catalogue**, and it records every
verdict and every step.

## The three kinds

| Kind | Who proposes | What reviewers do | Typical use |
|---|---|---|---|
| **Review** | a catalogue you already have, often from a pipeline | accept, reject or unsure, per event | checking detections |
| **Downlink** | each proposer, within a budget | vote for proposals | choosing burst data to download |
| **Labelling** | each proposer labels intervals | vote, like in a downlink | annotation campaigns, classes |

## How a round moves

<figure>
--8<-- "rounds.svg"
<figcaption>The steps of a round. Dashed arrows go back a step.</figcaption>
</figure>

A review round has its candidates already, so it starts at voting. The decider can also decide
straight from proposing, and the owner can reopen a decided round at either step.

## Open or blind

- In an **open** round, everyone in the round sees all verdicts.
- In a **blind** round, each reviewer sees only their own. The deciders and the project owners
  see everyone's. Use blind rounds when you want independent opinions: people don't follow
  the first verdict they see.

## Review a pipeline's detections

### 1. Put the detections in a catalogue

From Python, as in [Python and Jupyter](python.md):

```python
import cocat_rs as cc

project = cc.open("Flux ropes – MMS tail")
candidates = project.create_catalogue(name="Detector v3 output")
with project.transaction():
    candidates.add_events([
        project.create_event(start=s, stop=e, confidence=score, tags=["flux-rope"])
        for s, e, score in detections
    ])
project.save()
```

Or import a file (VOTable, tscat JSON, TFCat) into the project.

### 2. Start the round

In the browser, open **Rounds** in the project, then **New round**. Choose **Review
detections**. The form fills in the usual settings. Then:

- give it a name, like `Flux ropes v3 – check`;
- pick the catalogue to review;
- set a deadline if you want one;
- choose open or blind;
- check who reviews and who decides.

The round copies the catalogue. Your original stays as it is, and the pipeline can keep
writing to it.

From Python:

```python
review = project.create_round(
    "Flux ropes v3 – check",
    kind="review",
    visibility="blind",
    candidates=candidates,
    voters=["ana@lab.org", "ben@lab.org", "chloe@lab.org"],
    deciders=["you@lab.org"],
)
```

### 3. Reviewers give verdicts

Each reviewer opens **Rounds**. The rounds that need them come first, and each says what their
part is. They go through the table and give each event **accept**, **reject** or **unsure**.

The page shows how far they are, and jumps to the next event they haven't reviewed.

Reviewers who prefer code can give verdicts from Python, for instance after looking at the
data of each event:

```python
project.verdict(review["id"], event, "accept")
project.set_verdicts(review["id"], {uuid_1: "reject", uuid_2: "unsure"})
```

### 4. Decide

The decider sees how many reviewers accepted, rejected or were unsure for each event. They
pick the events to keep and move the round to **decided**. The kept events become the final
catalogue, and each carries how many accepted and rejected it.

The final catalogue says which round decided it. Anyone reading it later can open the round
and see the verdicts.

### Ask for changes

A round can go back a step, for example from voting to proposing, with a note in its log
("please add the March events"). That starts a new **revision**: earlier verdicts still show,
but no longer count. Reviewers see what they said last time and confirm or change it.

## Choose intervals for a downlink

A downlink round has a **budget**: a cap on what the chosen intervals cost, such as the data
volume in Gbit. In the form, pick the event attribute that sets the cost, for example `mode`,
and add a rate for each of its values: `burst` costs more per second than `survey`. Then set
the cap.

1. Each proposer adds intervals to their own proposal, a catalogue the round makes for them.
   The page shows what their proposal costs.
2. Voters look at all proposals and vote.
3. The decider builds the final selection. The page turns the total red when it goes over the
   cap.

## Tips

- **Write down what "accept" means** before the round starts, in the round's name or the
  project's files. "Accept = a clear bipolar Bz signature and a core field enhancement" saves
  long discussions later.
- **Use blind rounds** to measure agreement between reviewers, or between students.
- **Keep the rejected events.** They are training data for the next version of the pipeline.
