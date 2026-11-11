# Selection rounds: several people propose intervals, one catalogue comes out

## Why

Two workflows share one shape:

- **Selective downlink.** A team picks which intervals of burst data to bring down. Several
  people propose intervals; the selection must fit a telemetry budget; someone decides.
- **ML labelling.** Several people label the same data on their own, so the labels can be
  compared and a reference catalogue built with less bias than one person's labels.

Both are: people propose intervals on their own → the proposals are compared (and maybe
voted on) → a decision produces one catalogue. cocat-rs already has projects, roles, history
and catalogues; what is missing is the **round** that ties these steps together.

## What it is (decided by the user, 2026-11-11)

- A **selection round** lives inside a project.
- **Roles, not a fixed process.** The round says who may propose, who may vote and who
  decides. One person can hold all three. In a small group everyone proposes; in a larger
  one only a few may.
- **The vote is optional.** If nobody votes, the decider works directly from the proposals.
- **Visibility is a round setting**: *open* (everyone in the project sees proposals as they
  come in) or *blind* (a proposal is visible only to its author and the deciders until the
  round closes). It can change during a round.
- **Budget: one cap for the whole round.** Each interval's cost is a rate looked up from a
  category attribute of the event (e.g. `mode`: `burst` → 2 MB/min, `normal` → 0.1 MB/min),
  multiplied by the interval's duration. The selection's total must stay under the cap.
- **The budget is a guide, not a lock.** The page shows the total against the cap, in red
  when over; the decider can still go over. Intervals with no rate are listed, not refused.
- **Each final interval keeps who proposed it**: `proposed_by` is the list of everyone whose
  proposal had it (several people when several proposed it).
- **The budget is described with standard vocabularies**, the way SPASE 3.0 does (IHDEA
  2026): JSON-LD, values as QUDT `QuantityValue`, units as `qudt:Unit` carrying a VOUnits
  string in `skos:notation`. Only what no vocabulary covers (the rate table, the round) is
  our own field.
- **SciQLop is where people mark, compare and decide.** Grouping overlapping marks from
  different people into candidates is a SciQLop design problem, not the web page's.
- **The web page stays simple**: the round, who has submitted, the budget used, the final
  catalogue.

## Goals

- A project member with the right role can create a round, propose, vote, and decide —
  from Python (and so from SciQLop) first, from the web page for the simple parts.
- Each person's proposal is their own: nobody else can change it, and in a blind round
  nobody but its author and the deciders can read it before the round closes. Enforced by
  the server, not by clients being polite.
- Proposals are ordinary interval lists: start, stop, attributes (e.g. `mode`), the same
  model as a catalogue, so every existing reader (SciQLop, VOTable export, the web table)
  works on them.
- The final catalogue is an ordinary catalogue of the project. Each event records who
  proposed it and how many votes it got.
- Every client computes the same budget number: the cost calculation lives in
  `cocat-core`, exposed to Python.

## Non-goals (later, or someone else's)

- Grouping overlapping intervals into candidates, agreement scores, comparison views:
  SciQLop's job.
- A budget per time slot (per pass, per day).
- Weighted votes, ranked-choice voting, automatic decision rules ("keep everything with
  ≥ 2 votes"): the decider decides.
- Hiding who proposed what from the deciders.
- A SciQLop dashboard. The user said not to change SciQLop or the plugin yet; this design
  only fixes the formats and the Python API it will use.
- Proposing an IHDEA standard. Our own fields are kept in one clearly named place so that
  can happen later.

## Anti-goals (a failure even if it works)

- A proposer in a blind round can read another person's proposal before the round closes —
  through any route: socket, HTTP, history, search, export, links, copies.
- A proposal lost or changed after the round closed.
- A final catalogue nobody can trace back to who proposed and who decided.
- Two clients showing different budget numbers for the same selection.

## Shape (decided parts)

### The round

A round has: a name, its project, a state, a visibility, an optional deadline, its roles,
its budget (optional), and once decided, its final catalogue.

States, in order:

1. **proposing** — proposers write their proposals.
2. **voting** — proposals are frozen and visible to all project members (blind or not);
   voters vote. A decider can skip this state.
3. **decided** — the final catalogue exists; nothing in the round changes any more.

A decider moves the round forward. A deadline moves *proposing* to *voting* by itself.
**A project owner can move a round back** to an earlier state (the user, 2026-11-11: "we
should not be blocked by a machine"): this unfreezes proposals or votes, and is recorded in
the round's history with who did it.

Roles: lists of project members per round role (proposer, voter, decider). *(My call:)* by
default every owner and editor proposes and votes, and the owner decides; viewers may be
named proposers or voters too, since labelling in a round is not editing the project.

### Proposals

One per proposer, created on their first write. A proposal is its own document in the
project (`{project}/{doc}`, the existing per-document storage, socket and history), laid
out as a catalogue document holding one catalogue. Only its author writes it, and only in
*proposing*. Who may read it follows the visibility rule above.

### Votes

A vote is one voter's set of supported event uuids, across all proposals. Stored per voter;
only that voter changes it, only in *voting*. Counting votes over candidates is SciQLop's
job; the server only stores and serves them.

### Decision

The decider writes the final catalogue into the project's catalogues document, as an
ordinary catalogue, and marks the round decided with that catalogue's id. Each final event
carries `proposed_by` (list of people) and `votes` (count) attributes, and the round's
description (below) is stored on the catalogue so the budget travels with it.

### The budget description

```json
{
  "@context": {
    "qudt": "https://qudt.org/schema/qudt/",
    "skos": "http://www.w3.org/2004/02/skos/core#",
    "dcterms": "http://purl.org/dc/terms/",
    "cocat": "https://cocat.sciqlop.fr/ns/round#"
  },
  "@type": "cocat:Budget",
  "cocat:attribute": "mode",
  "cocat:cap": {
    "@type": "qudt:QuantityValue", "qudt:numericValue": 500,
    "qudt:hasUnit": { "@type": "qudt:Unit", "skos:notation": "Mibyte",
      "dcterms:conformsTo": { "@id": "https://www.ivoa.net/documents/VOUnits/" } }
  },
  "cocat:rates": [
    { "cocat:value": "burst",
      "cocat:rate": { "@type": "qudt:QuantityValue", "qudt:numericValue": 2.0,
        "qudt:hasUnit": { "@type": "qudt:Unit", "skos:notation": "Mibyte/min",
          "dcterms:conformsTo": { "@id": "https://www.ivoa.net/documents/VOUnits/" } } } },
    { "cocat:value": "normal",
      "cocat:rate": { "@type": "qudt:QuantityValue", "qudt:numericValue": 0.1,
        "qudt:hasUnit": { "@type": "qudt:Unit", "skos:notation": "Mibyte/min",
          "dcterms:conformsTo": { "@id": "https://www.ivoa.net/documents/VOUnits/" } } } }
  ]
}
```

- `cocat:` is our namespace; its IRI is a placeholder until the project picks a stable one.
- A rate's unit must be the cap's unit divided by a time unit; anything else is refused
  when the budget is set, with the reason.
- *(My call:)* a fixed cost per interval (not per time) is written as a rate whose unit has
  no time divisor (`Mibyte`), and is not multiplied by the duration.

### Cost

`cocat-core` computes, for a list of events and a budget: the total cost, the cap, and the
events it could not price (attribute missing, or a value with no rate), listed rather than
counted as zero. Exposed to Python. The web page and SciQLop show "320 of 500 MiB used,
3 intervals without a rate".

## Calls made after the builder check (mine, not the user's — check them)

- **Who creates and decides.** Owners and editors create rounds. A decider must be an owner
  or editor, since deciding writes the project's catalogues; proposers and voters may be
  viewers. The default decider is the round's creator.
- **The final catalogue is trusted to the decider.** SciQLop merges overlapping marks, so a
  final event need not match any one proposal: the server cannot derive `proposed_by` and
  `votes`, and does not check them. Traceability comes from history (who wrote the final
  catalogue) and from the frozen proposals and votes, which stay readable.
- **Who reads a blind proposal.** Its author, the round's deciders and the project's owners
  (an owner can make themselves a decider anyway). Server superusers get no extra access,
  as today. Until the round leaves *proposing*, every project-wide route — history, search,
  export, links, copies, the state download — skips proposal documents the caller may not
  read. One server function answers "may this user read this document now", and every
  route asks it.
- **Frozen is enforced by the server.** After *proposing*, the server refuses updates to
  proposals; after *voting*, to votes; an offline client's late edits are refused, with the
  reason, not merged. Only an owner moving the round back unfreezes them.
- **Votes follow the proposals' visibility rule**: in a blind round a vote is visible only to
  its voter and the deciders until the round is decided.
- **Budget and roles can change until the round is decided.** Numbers are computed live
  from the current budget, so every client agrees. A proposer removed from the list keeps
  their proposal, frozen and still readable by the deciders.
- **Units convert.** Byte and bit, with decimal and binary prefixes, convert between each
  other, and so do `s`, `min`, `h`, `d`. Only VOUnits spellings (`Mibyte`, not `MiB`). A
  zero-length interval costs zero.
- **The round's description lives in a catalogue attribute** (`round`, holding the JSON-LD
  text), so it needs no new catalogue field; VOTable and tscat exports carry it like any
  attribute.

## Left to the builder

- The database tables for rounds, roles and votes; the API routes and their names.
- How per-document read access is enforced (the server today gives every project member
  every document of the project).
- The unit parser: the smallest VOUnits subset that covers bytes and bits with decimal and
  binary prefixes, divided by `s`, `min`, `h` or `d`; a `simplify:` comment naming what it
  does not parse.
- The Python API's names, following the existing client (`Project`, `Client`).
- The web page's layout.
