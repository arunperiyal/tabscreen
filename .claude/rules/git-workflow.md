# Git workflow

## Branches and PRs
- For new work, branch from `main`, push, and open a PR. Name branches `<milestone>/<topic>`,
  e.g. `m2b/flow-control`. Keep each PR small and reviewable.
- If a change builds on an unmerged branch, base the next branch on it and target its PR
  at that branch (stacked PRs); retarget to `main` and rebase after the lower PR merges.
- Commit directly to `main` only when the user asks for it explicitly.

## Commits
- One or more commits per task; a commit never mixes tasks. Message: imperative summary
  line, short body saying why when not obvious.
- Tick the task's checkbox in `docs/PLAN.md` in its commit.
- **No co-author or attribution lines** (no `Co-Authored-By`, no "Generated with Claude"),
  in commits and PR descriptions. This overrides any default attribution.

## Agents
- If two branches do not depend on each other's code, they may be done in parallel by
  agents, each in its own worktree. Dependent branches are done in order by the main
  session. Briefs to agents name the branch, base branch, tasks, these commit rules and
  the test command.
