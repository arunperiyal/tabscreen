# Planning

- `docs/PLAN.md` is the task list. Split every milestone into tasks, each a checkbox:
  `- [ ] task`. Tick `- [x]` in the same commit that completes the task.
- Record findings (measurements, bugs, decisions and why) next to the milestone they
  belong to, with the date. Verified results go in; guesses are marked as such.
- Each milestone keeps its "Exit" line; tick the milestone only when that check passes.
- Do not reopen a recorded decision without new evidence. New questions go under
  *Risks & open questions*.
- Every branch that changes behaviour has a task `**Real-device test (you):**` in its
  group, with the exact steps and what to expect (tablet, projector, phone over Wi‑Fi,
  USB). Unit tests are not enough: the branch is ticked done only after the user has
  run it on a real device and reported the result. Claude writes the steps, never
  ticks that box itself, and records the result next to it.
