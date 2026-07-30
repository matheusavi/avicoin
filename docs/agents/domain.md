# Domain Docs

How the engineering skills should consume this repo's domain documentation.

This is a **single-context** repo. There is no `CONTEXT-MAP.md` and no
per-package contexts.

## Before exploring, read these

- **[`docs/glossary.md`](../glossary.md)** — the ubiquitous language. This repo
  keeps its glossary here rather than at the conventional root `CONTEXT.md`;
  where a skill says "read `CONTEXT.md`", read this file.
- **[`docs/adr/`](../adr/)** — read the ADRs touching the area you are about to
  work in. Start with [ADR-0001](../adr/0001-v1-scope.md), which bounds what v1
  is and therefore which other decisions still apply.
- **[`docs/ARCHITECTURE.md`](../ARCHITECTURE.md)** — the target design and the
  invariants that hold across every module.

If a file does not exist, **proceed silently**. Don't flag its absence or suggest
creating it upfront. `/domain-modeling` (reached via `/grill-with-docs` and
`/improve-codebase-architecture`) creates them lazily, when terms or decisions
actually get resolved.

## File structure

```
/
├── CLAUDE.md                    ← how the code looks today, for an agent editing it
└── docs/
    ├── glossary.md              ← the ubiquitous language (this repo's CONTEXT.md)
    ├── ARCHITECTURE.md          ← target design + invariants
    ├── agents/                  ← this configuration
    └── adr/
        ├── README.md            ← index + the decisions still open
        ├── TEMPLATE.md          ← not an ADR; holds no number
        └── NNNN-<slug>.md
```

## Use the glossary's vocabulary

When your output names a domain concept — an issue title, a refactor proposal, a
hypothesis, a test name — use the term as defined in `docs/glossary.md`. Don't
drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal: either you're
inventing language the project doesn't use (reconsider), or there's a real gap
(note it for `/domain-modeling`).

## ADR numbers are assigned on write, never reserved

**A new ADR takes the next free number, whatever its topic.** Numbers are
append-only; existing ADRs are never renumbered, and no number is ever held for a
decision that has not been made.

Reserving numbers was tried and abandoned. It collided inside a single session —
one grilling round produced two ADRs, which knocked every later reservation out
of alignment and left one decision carrying an invented id. So: **an undecided
topic is referred to by name, never by a number.** In the glossary that looks like
`⏳ (pending — monetary model)`, not `⏳ (ADR-0006)`.

The open topics are listed in `docs/adr/README.md` under "Open decisions". When
one is settled, it takes the next free number and moves up to the index.

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than
silently overriding:

> _Contradicts ADR-0001 (output locking model) — but worth reopening because…_

A `Proposed` ADR is **not** a decision. Treat its recommendation as the current
default, but say so when you rely on it.
