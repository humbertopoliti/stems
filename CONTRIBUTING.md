# Contributing to stems

Thanks for your interest! Bug reports, feature ideas and pull requests are all
welcome. By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).

## Before you start

- **Bugs:** open an issue with the bug template. `stems --version --json`
  output and a minimal `stems.yaml` that reproduces the problem help a lot.
- **Features and larger changes:** open an issue first so we can agree on the
  approach before you invest time in a PR.
- **Security issues:** don't open a public issue; see [SECURITY.md](SECURITY.md).

## Development setup

You need:

- Rust: the toolchain is pinned in `rust-toolchain.toml`; `rustup` installs it
  on first use.
- Python 3 (example services, YAML lint, test tooling).
- Docker with compose v2 only for the Docker tier (`make check-docker`);
  everything else runs without it.

```sh
make ci      # what CI runs: fmt, clippy, unit and in-process tests, yaml lint
make check   # the full local suite: also process tests, example services, e2e
make build   # release build: target/release/stems
make docs    # regenerate docs/cli.md after changing CLI flags or help text
```

CI runs only the regression subset (`make ci`): nothing that starts processes,
the example services or Docker. Run `make check` locally before opening a PR
that touches the runtime, daemon or lifecycle. End-to-end scenarios live in
`tests/features/` (cucumber); run one file with
`make e2e FEATURE=tests/features/foo.feature`.

## Pull requests

- Keep PRs focused: one change per PR.
- Add or update tests: unit tests next to the code, an e2e scenario for
  user-visible behaviour.
- Update the docs in `docs/` when behaviour changes.
- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org)
  (`feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`); the changelog is
  generated from them with git-cliff. PRs are squash-merged, so the PR title
  becomes the commit message: make it a conventional one.

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE).
