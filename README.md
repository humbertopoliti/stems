# stems

Developers working on large systems routinely need five to twenty things running locally at once: databases in containers, message brokers, several backend services, a couple of frontends, a worker or two. Today this knowledge lives in READMEs, shell aliases, half-maintained compose files and people's heads. **stems** makes the local environment a first-class, versioned, shareable artifact. You describe every running thing ("stem"), how it is built, configured, seeded, started, watched and connected. Then one command brings the whole system up, one brings it down, and a dashboard shows you what is healthy. Everything the dashboard shows is also available as structured data, so AI agents can operate the environment the same way a human does.

## Status

Early development. `stems --version` is the only command so far.

## Install

```sh
brew install humbertopoliti/tap/stems          # macOS (Apple Silicon and Intel): binary, completions, man pages
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/humbertopoliti/stems/releases/latest/download/stems-cli-installer.sh | sh   # macOS/Linux
stems upgrade                         # later: brew upgrade, or the installer command
```

No release has been published yet. Tarballs, uninstalling and what the release smoke test checks: [`docs/install.md`](docs/install.md). Maintainers: [`release/RELEASING.md`](release/RELEASING.md).

## Development

```sh
make check   # fmt, clippy -D warnings, tests, python tests, yaml lint, e2e, trace
make build   # release build of the `stems` binary
```
