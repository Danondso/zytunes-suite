# Security policy

## Reporting a vulnerability

Please **do not** open a public issue for security bugs.

- Once this repository is public, use GitHub's **Report a vulnerability**
  (Security → Advisories) so we can coordinate a fix before it is disclosed.
- Until then, email the maintainer listed on the GitHub profile.

Include the affected crate or binary (`zytunes`, `zytunes-tui`,
`zytunes-serve`, `zune-mtp`, `ipod-db`), a reproduction if you have one, and
the impact.

## What this project does not ship

- **MTPZ handshake credentials** (`.mtpz-data`) are not in the repo. Zune
  connect needs a copy you provide locally; iPod sync does not.
- **Stream tokens** belong in a local `.env` / `config.toml`, never in git.
  `zytunes-serve` refuses to start without a non-empty token.

## Dependency advisories

PRs and pushes to `main` run `cargo deny check` (the **security** job in the
CI workflow). Known exceptions live in `deny.toml`.
