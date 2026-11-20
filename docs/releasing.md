# Releasing

Every push to `main` that passes CI becomes a release (ADR 0014). It reaches early testers first and everyone else about two days later.

## Publishing a release

1. Add a `## 0.1.N - YYYY-MM-DD` section at the top of `CHANGELOG.md`, where `N` is the newest `v0.1.N` tag plus one, with `### Major` and `### Minor` lists. Keep each line short and in plain words; this section becomes the release notes.
2. Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` locally, then push to `main`.
3. Check that the CI and Release workflows finish green. Release installs the new installer on clean Windows 2022 and 2025 machines (fresh install, update over the current release, uninstall; `scripts/install-test.ps1`) before publishing; if that fails the release stays a draft. The next push replaces that draft.
4. Check the new release on GitHub: it is a **prerelease**, and the `early` release's `latest.json` names its version. Copies with **Get new versions early** switched on now offer it.
5. Nothing else to do: the daily Promote workflow marks the release as latest for everyone once it is 48 hours old.

## When a release is bad

1. Edit the release on GitHub and add a line starting with `HOLD` to its notes. Promote then skips it, so it never reaches everyone.
2. Fix forward: release a new version with the fix. The updater never installs an older version, so early testers move on only by getting a newer one.
3. There is no need to remove `HOLD` afterwards: the next version replaces the held one on both channels.
