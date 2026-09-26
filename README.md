# stems

Developers working on large systems routinely need five to twenty things running locally at once: databases in containers, message brokers, several backend services, a couple of frontends, a worker or two. Today this knowledge lives in READMEs, shell aliases, half-maintained compose files and people's heads. **stems** makes the local environment a first-class, versioned, shareable artifact. You describe every running thing ("stem"), how it is built, configured, seeded, started, watched and connected. Then one command brings the whole system up, one brings it down, and a dashboard shows you what is healthy. Everything the dashboard shows is also available as structured data, so AI agents can operate the environment the same way a human does.

## Status

Early development. `stems --version` is the only command so far.

## Development

- Requirements and architecture: [`REQUIREMENTS.md`](REQUIREMENTS.md)
- Delivery plan and agent workflow: [`plan/README.md`](plan/README.md) (conventions in [`plan/DECISIONS.md`](plan/DECISIONS.md))

```sh
make check   # fmt, clippy -D warnings, tests, python tests, yaml lint, e2e, trace
make build   # release build of the `stems` binary
```
