# Contributing

Thanks for your interest! Bug reports, ideas and pull requests are welcome.

## Workflow

- `main` is protected: every change goes through a pull request, CI must pass, history stays
  linear (squash or rebase merge) and force pushes are rejected.
- **Commits must be signed** (SSH or GPG) — see
  [GitHub's guide](https://docs.github.com/en/authentication/managing-commit-signature-verification/signing-commits).
- Use [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `test:`,
  `refactor:`, `chore:`…) and explain *why* in the message body.

## Development

Build dependencies (Debian/Ubuntu/Pop!_OS):

    sudo apt install pkg-config libxkbcommon-dev libwayland-dev libudev-dev libinput-dev libfontconfig-dev just

Checks run by CI, to run before pushing:

    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test

Try your build in the panel with `just install`, then add the applet from
Settings → Desktop → Panel → Applets.

## Guidelines

- Behaviour changes come with tests, edge cases included. Accounting, persistence and formatting
  logic lives in pure modules (`usage`, `store`, `control`, `format`) so it can be unit tested;
  keep `app.rs` a thin UI layer.
- Never weaken a test, a lint or a CI check to make it pass; open an issue instead.
- Keep diffs focused; unrelated refactors belong in their own pull request.
