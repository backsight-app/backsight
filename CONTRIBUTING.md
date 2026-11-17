# Contributing to Backsight

Thanks for helping. Setup and commands are in [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Before you open a pull request

- `cargo fmt --all`, `cargo clippy --workspace --all-targets` and `cargo test --workspace` pass
- `npm run build` in `app/` passes
- New settings have a glossary entry written to [brand-and-voice.md](docs/design/brand-and-voice.md)
- UI changes include an updated screenshot if they change what the README shows
- Hard-to-reverse decisions have an [ADR](docs/adr/README.md)

## Pull requests

Keep them focused: one change per PR, titled as a Conventional Commit (`feat(ui): explain look-ahead`). Describe what changed, why, and how you tested it. The PR template has a checklist.

## License

Backsight is licensed under the GNU General Public License, version 3 or later ([LICENSE](LICENSE)). By contributing, you agree that your contributions are licensed under GPL-3.0-or-later too.

If you add a dependency, run `scripts/third-party-notices.ps1` and commit the updated `THIRD-PARTY-NOTICES.txt`, so its authors are credited.

## Reporting bugs

Use the bug report template. Include your Windows version, GPU and driver version, the encoder and container you used, and `%APPDATA%\Backsight\logs\backsight.log` if there is one.
