# TabScreen

Rust host (`host/`) plus a web client (`web/`) that turns an Android tablet into a
touch monitor for GNOME/Wayland. Roadmap and findings: `docs/PLAN.md`. Design:
`docs/ARCHITECTURE.md`. Build and test: `docs/DEVELOPMENT.md`.

## Essentials
- The host talks to GNOME only through Mutter's D-Bus API (isolated in `mutter.rs`).
- The web client is embedded in the binary; rebuild after editing `web/`.
- Never commit secrets: `~/.config/tabscreen/` (token, key, certificate) stays out of the repo.
- Run `cargo clippy` and `cargo test` in `host/` before committing.

## Rules (read the relevant file before acting)
- @.claude/rules/planning.md: milestones and checkboxes in `docs/PLAN.md`
- @.claude/rules/git-workflow.md: branches, commits, PRs, no co-author lines
- @.claude/rules/documentation.md: documentation guidelines
- @.claude/rules/session-notes.md: `SESSION.md` is updated only on request
