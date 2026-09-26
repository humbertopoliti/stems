# Installing stems

stems ships as a single binary, `stems` (the workspace daemon is the same
binary: `stems daemon`). Releases are built by
[cargo-dist](https://github.com/axodotdev/cargo-dist) from a `v*` tag for:

| Target | Runner that builds and smoke-tests it |
|---|---|
| `aarch64-apple-darwin` (Apple Silicon) | `macos-14` build, `macos-latest` brew smoke |
| `x86_64-apple-darwin` (Intel Mac) | `macos-15-intel` build and brew smoke |
| `x86_64-unknown-linux-gnu` | `ubuntu-22.04` build, installer.sh smoke |
| `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm` build (no smoke) |

Windows is out of scope. `<org>` below is the GitHub owner of the stems
repository (`humbertopoliti` today).

> **Status:** the pipeline is configured and verified locally
> (`dist plan`, `make smoke`, `make size`) but has not published a release
> yet: the repository has no GitHub remote. The commands below are what users
> run once `v0.1.0-rc.1`/`v0.1.0` exist.

## Homebrew (macOS, recommended)

```sh
brew install <org>/tap/stems
```

This taps `github.com/<org>/homebrew-tap` and installs:

- `bin/stems`;
- shell completions for bash, zsh and fish (`share/zsh/site-functions/_stems`,
  `etc/bash_completion.d/stems`, `share/fish/vendor_completions.d/stems.fish`);
- man pages, one per command: `man stems`, `man stems-up`,
  `man stems-daemon-start`, …

`brew test stems` checks `stems --version` and validates a tiny embedded
workspace. Homebrew binaries are not notarised; Homebrew does not need that.

## installer.sh (macOS and Linux)

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/<org>/stems/releases/latest/download/stems-cli-installer.sh | sh
```

Installs `stems` into the first of `$XDG_BIN_HOME`, `$XDG_DATA_HOME/../bin`,
`~/.local/bin`, adds it to your shell's PATH (set
`INSTALLER_NO_MODIFY_PATH=1` to skip that) and writes a receipt to
`~/.config/stems-cli/`. `releases/latest` never points at a prerelease: for an
`-rc`, use `releases/download/v0.1.0-rc.1/stems-cli-installer.sh`.

The installer only installs the binary. Add completions yourself:

```sh
stems completions zsh  > ~/.zfunc/_stems            # with fpath+=(~/.zfunc)
stems completions bash > ~/.local/share/bash-completion/completions/stems
stems completions fish > ~/.config/fish/completions/stems.fish
```

## Tarballs

Every release has `stems-cli-<target>.tar.xz`, a `.sha256` next to each and
a combined `sha256.sum`. Each tarball holds:

```
stems-cli-<target>/
  stems
  README.md
  completions/{stems.bash,_stems,stems.fish}
  man/stems.1, man/stems-up.1, …
```

```sh
shasum -a 256 -c stems-cli-aarch64-apple-darwin.tar.xz.sha256
tar -xJf stems-cli-aarch64-apple-darwin.tar.xz
install -m 755 stems-cli-aarch64-apple-darwin/stems ~/.local/bin/
cp stems-cli-aarch64-apple-darwin/man/*.1 ~/.local/share/man/man1/   # optional
```

## From source

```sh
cargo install --locked --git https://github.com/<org>/stems stems-cli
```

(The crate is not on crates.io yet; the name `stems` was free there on
2026-09-26, see REQUIREMENTS.md §11 Q1.)

## Upgrading

```sh
stems upgrade            # does the right thing for how you installed
stems upgrade --dry-run  # only print what it would run
```

`stems upgrade` looks at where the running binary lives:

| Install method | Detected by | `stems upgrade` |
|---|---|---|
| `brew` | path under a Homebrew `Cellar/` (or `/opt/homebrew`, `/home/linuxbrew/.linuxbrew`) | runs `brew upgrade stems` |
| `cargo` | binary in `$CARGO_HOME/bin` (`~/.cargo/bin`) | prints the `cargo install` command |
| `tarball` | anything else (installer.sh, an unpacked tarball, a source build) | prints the installer.sh one-liner |

`stems --version --json` reports the same `install_method`;
`stems upgrade --dry-run --json` gives `data: { install_method, command }`.

### After an upgrade: version compatibility

- **CLI ↔ daemon.** A running workspace daemon keeps the old version. The new
  CLI refuses to drive it (`DAEMON_VERSION_MISMATCH`, exit 4) with the hint
  to restart it; `stems daemon stop` works across versions, and the next
  command starts a daemon of the new version. Your stems keep running only
  if you bring them back up: `stems daemon stop && stems up --detach`.
- **Workspace format.** `stems.yaml` declares `schema_version`. A binary that
  does not understand it refuses the workspace with
  `SCHEMA_VERSION_UNSUPPORTED` (exit 2), naming the stems version required
  (`schema_version 2 requires stems >= 0.4.0` when it knows the format,
  `requires stems > 0.1.0` for a newer one) and its supported versions. The
  table of `schema_version → first stems release` is
  `stems_config::compat::SCHEMA_VERSIONS`.

| `schema_version` | Minimum stems |
|---|---|
| 1 | 0.1.0 |

## Uninstalling

Stop every workspace daemon first (`stems down --all` in each workspace),
then:

```sh
brew uninstall stems && brew untap <org>/tap       # Homebrew
rm ~/.local/bin/stems && rm -rf ~/.config/stems-cli # installer.sh
cargo uninstall stems-cli                           # cargo
```

State lives in `STEMS_HOME` (default `~/Library/Application Support/stems` on
macOS, `$XDG_STATE_HOME/stems` or `~/.local/state/stems` on Linux): remove it
to forget recorded runs and logs. Workspaces keep `.stems/` directories (git
checkouts and stamps) that you can delete per workspace.

## What the release smoke test proves

Every tag runs `release/smoke.sh` against the **just-built** artifacts before
the GitHub Release is created or the tap is touched
(`.github/workflows/release-smoke.yml`); a failure stops the release.

On macOS (Apple Silicon and Intel) the formula is rendered against the local
tarballs, installed with `brew install` from a throwaway tap, and
`brew test`ed. On Linux (x86_64) the tarballs are served on localhost and
installed with the real `installer.sh`. Then, on each:

1. `stems --version` and `--version --json` work, and `install_method` is right
   (`brew` for the brew install);
2. `stems init --from hello-shop` scaffolds a workspace that validates;
3. the `examples/` minimal workspace comes up detached (`stems up --detach
   echo-svc`), `stems status --json` reports every stem healthy, and
   `stems down --all` stops it and the daemon;
4. nothing leaks: every started pid is gone, no `stemsd.sock`/`stemsd.lock`
   is left under `STEMS_HOME`, and no `app.py`/`serve.py`/`worker.py` process
   is running;
5. with brew: `_stems` is installed under
   `$(brew --prefix)/share/zsh/site-functions` and `man stems` /
   `man stems-up` render.

It does **not** prove: Docker/compose stems on macOS (runners have no Docker;
`SMOKE_DOCKER=1 release/smoke.sh` covers the full hello-shop locally), the
`aarch64-unknown-linux-gnu` binary at runtime, or the published formula's
download URLs (those are exercised by the first real `brew install`).

Run it yourself: `make smoke` (builds `target/release/stems`), or
`make smoke BIN=$(brew --prefix)/bin/stems SMOKE_BREW=1` after a brew install.
