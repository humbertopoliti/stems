# Releasing stems

Maintainer notes for the release pipeline (deliverable 32). User-facing
install docs: [`docs/install.md`](../docs/install.md).

## Pieces

| File | Role |
|---|---|
| `dist-workspace.toml` | cargo-dist config: targets, tarballs, installer.sh, hooks |
| `crates/stems-cli/Cargo.toml` `[package.metadata.dist] dist = true` | the only package dist ships |
| root `Cargo.toml` `[profile.dist]` | the profile `dist build` uses (inherits `release`: strip + thin LTO); required, dist fails without it |
| `.github/workflows/release.yml` | **generated** by `dist generate`; never edit by hand |
| `.github/build-setup.yml` | spliced into the build job: runs `release/build-extras.sh` |
| `release/build-extras.sh` | completions + man pages (`stems __man`) into `target/dist-extras/` (packed by `include`) |
| `.github/workflows/release-smoke.yml` | gate before the GitHub Release: brew smoke on both Macs, installer.sh smoke on Linux |
| `.github/workflows/publish-homebrew.yml` | after the release: render and push `Formula/stems.rb` to the tap |
| `.github/workflows/release-changelog.yml` | after the announcement: regenerate and commit `CHANGELOG.md` |
| `release/formula/stems.rb.tmpl`, `release/render_formula.py` | the formula (golden: `release/tests/`) |
| `release/smoke.sh` | the smoke test (`make smoke`) |
| `release/set-version.sh` | bump the workspace version before tagging |
| `cliff.toml` | git-cliff config for `CHANGELOG.md` |

Pipeline for a tag `vX.Y.Z[-rc.N]`:

```
plan → build-local-artifacts (4 native runners: build-extras.sh, dist build)
     → build-global-artifacts (installer.sh, sha256.sum)   ┐
     → custom-release-smoke (brew ×2 macOS, installer.sh Linux) ┘→ host (GitHub Release)
     → custom-publish-homebrew (tap: default branch, or `rc` for prereleases)
     → announce → custom-release-changelog (stable only)
```

## One-time setup

1. Create the GitHub repository and push: `git remote add origin
   git@github.com:<org>/stems.git && git push -u origin main`. If `<org>` is
   not `humbertopoliti`, update `repository` in the root `Cargo.toml` (it
   feeds installer URLs, `stems upgrade` and the formula homepage) and run
   `dist generate`.
2. Create the tap repository `<org>/homebrew-tap` (public, with a README and
   an empty `Formula/` directory, default branch `main`).
3. Create a fine-grained token with *Contents: read and write* on the tap
   only; store it as the repository secret `HOMEBREW_TAP_TOKEN`.
4. Optional: repository variable `HOMEBREW_TAP_REPO` if the tap is not
   `<owner>/homebrew-tap` (the only place the tap is named).
5. Settings → Actions → General → Workflow permissions: *Read and write* (the
   changelog job pushes to the default branch; allow it through branch
   protection or drop that job).

## Cutting `v0.1.0-rc.1`

```sh
cargo install cargo-dist --locked   # provides `dist`, v0.32.0
cargo install git-cliff --locked

make check && make size && make smoke     # local gates
dist plan                                 # what the tag will build

release/set-version.sh 0.1.0-rc.1             # dist needs tag version == package version
dist plan --tag v0.1.0-rc.1                   # "announcing v0.1.0-rc.1", prerelease
git cliff --tag v0.1.0-rc.1 -o CHANGELOG.md   # notes for the GitHub Release
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "chore(release): v0.1.0-rc.1"
git tag -a v0.1.0-rc.1 -m "stems 0.1.0-rc.1"
git push origin main v0.1.0-rc.1
```

cargo-dist only announces a tag whose version equals the package version
(verified: with `0.1.0` in Cargo.toml, `dist plan --tag v0.1.0-rc.1` says
"doesn't have anything for dist to Release"), hence `set-version.sh`; the
`-rc.1` suffix makes it a prerelease, which the publish job sends to the
tap's `rc` branch. Watch the *Release* workflow. Then:

```sh
brew tap <org>/tap
git -C "$(brew --repository <org>/tap)" fetch origin rc
git -C "$(brew --repository <org>/tap)" checkout rc
brew install <org>/tap/stems && brew test stems
make smoke BIN="$(brew --prefix)/bin/stems" SMOKE_BREW=1
git -C "$(brew --repository <org>/tap)" checkout main
```

## Cutting `v0.1.0` (stable)

Same as above with `release/set-version.sh 0.1.0` and the tag `v0.1.0`: the formula lands on the tap's default
branch, so `brew install <org>/tap/stems` gets it, and the changelog job
commits the regenerated `CHANGELOG.md`.

## Changing the pipeline

- Edit `dist-workspace.toml` or the three custom workflows, then
  `dist generate` (CI's `release.yml` is checked by `dist plan`; a stale one
  fails the plan job).
- Formula change: edit `release/formula/stems.rb.tmpl`, then
  `STEMS_UPDATE_GOLDEN=1 make test-release` and review the golden diff.
- New `schema_version`: add a row to `stems_config::compat::SCHEMA_VERSIONS`
  (`(2, "0.4.0")`) and to the table in `docs/install.md`.

## Not covered yet

- macOS signing/notarisation (Homebrew does not require it; the installer.sh
  path may trip Gatekeeper on first run if the binary is ever quarantined).
- Bottles: the formula installs the prebuilt tarball (no `brew bottle` step).
- A universal2 macOS binary (per-arch tarballs instead).
