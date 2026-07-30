# Issue tracker: GitHub

Issues, specs (PRDs), and tickets for this repo live on **GitHub** —
[matheusavi/avicoin](https://github.com/matheusavi/avicoin). Use the `gh` CLI for
all operations. Infer the repo from `git remote -v`; `gh` does this automatically
inside a clone.

**Docs live in the repo, work lives on GitHub.** Architecture, decisions, and
vocabulary are Markdown under `docs/`. Anything with a *state* — a spec, an epic,
a ticket — belongs on GitHub. Do not add a planning document to `docs/` that
duplicates a milestone or an issue.

## Milestones are the epics

The v1 plan is seven milestones, defined in
[ADR-0001](../adr/0001-v1-scope.md) and created on GitHub. A milestone is the
unit of planning; tickets are cut from one when it is picked up, not in advance.

- **List**: `gh api repos/matheusavi/avicoin/milestones --jq '.[] | {number, title, description}'`
- **Assign an issue**: `gh issue edit <n> --milestone "<title>"`
- New work must land in a milestone, or explain why it is out of ADR-0001's scope.

## Conventions

- **Create an issue**: `gh issue create --title "..." --body "..."` — use a
  heredoc for multi-line bodies. Always set `--milestone`.
- **Read an issue**: `gh issue view <number> --comments`
- **List issues**: `gh issue list --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'`
  with appropriate `--label` and `--milestone` filters.
- **Comment**: `gh issue comment <number> --body "..."`
- **Label**: `gh issue edit <number> --add-label "..."` / `--remove-label "..."`
- **Close**: `gh issue close <number> --comment "..."`

### Blocking edges

Use GitHub's **native issue dependencies** — the canonical, UI-visible form:

```
gh api --method POST repos/matheusavi/avicoin/issues/<child>/dependencies/blocked_by \
  -F issue_id=<blocker-db-id>
```

`<blocker-db-id>` is the blocker's numeric **database id**
(`gh api repos/matheusavi/avicoin/issues/<n> --jq .id`) — *not* the `#number` and
not the `node_id`. GitHub reports `issue_dependencies_summary.blocked_by`, which
counts open blockers only, so it is the live gate. Where dependencies are
unavailable, fall back to a `Blocked by: #<n>, #<n>` line at the top of the
child's body. A ticket is unblocked when every blocker is closed.

## Project board

The repo has a GitHub Project board for cross-milestone views. Project (v2)
operations need the `project` OAuth scope, which the default `gh` token does not
carry. If `gh project` returns a missing-scope error, the human must run
`gh auth refresh -s project,read:project` — it is interactive, so ask rather than
attempting it.

Milestones and issues work with plain `repo` scope and are the fallback for
everything the board would otherwise show.

## Pull requests as a triage surface

**PRs as a request surface: no.** _(Set to `yes` if this repo starts treating
external PRs as feature requests; `/triage` reads this flag.)_

GitHub shares one number space across issues and PRs, so a bare `#42` may be
either — resolve with `gh pr view 42`, falling back to `gh issue view 42`.

## When a skill says…

- **"publish to the issue tracker"** → create a GitHub issue, with a milestone.
- **"fetch the relevant ticket"** → `gh issue view <number> --comments`.
