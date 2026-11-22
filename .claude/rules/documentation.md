# Documentation guidelines

- Every public module and function has a doc comment: what it does, arguments, return,
  units and coordinate conventions (device vs logical pixels). Keep them short.
- User-facing documentation lives in `README.md` and `docs/`; design in `docs/ARCHITECTURE.md`,
  roadmap in `docs/PLAN.md`, fixes for common problems in `docs/TROUBLESHOOTING.md`.
- A feature is not done until its docs are updated in the same PR: `--help` text, the
  README option table, and the architecture/protocol notes if they changed.
- Write plain, direct English; explain the why of a design choice, not only the what.
- Document every command-line option with type, default and an example. Keep names
  identical to the code.
