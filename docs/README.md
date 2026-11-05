# stems documentation

Start with [What stems can do](features.md): every capability, the commands
that use it, the `stems.yaml` keys that configure it and the page below that
documents it. The same index is in the terminal: `stems features` (`--json`
for agents), `stems docs` lists these pages and `stems docs <topic>` prints
one; every `stems <command> --help` ends with a "See also" line.

## Start here

- [What stems can do](features.md): the feature index, by area.
- [Getting started](getting-started.md): onboard a real system step by step.
- [Installing stems](install.md): Homebrew, installer, tarballs, upgrading, completions.

## Config

- [Config](config.md): profiles, local overrides, include / extends, reload on change, environment and `env_files`, secrets, outputs, variants, configurations.
- [Ports](ports.md): named ports, `auto`, conflicts, port variables, `stems ports`.

## Lifecycle

- [Lifecycle](lifecycle.md): states, readiness, `up` / `down` / `start` / `stop` / `restart` / `status`, a stem's environment.
- [Process model](process-model.md): process groups, stopping, pid-reuse defence.
- [Crash recovery and orphans](recovery.md): the state file, adoption, orphans.
- [Health checks and the status model](health.md): probe types, transitions, glyphs, `stems health`.
- [Restart policies and backoff](restart.md): policies, backoff, cascading restarts.
- [Watchdogs](watchdogs.md): file watchers that restart, rebuild or run a script.

## Scripts & context

- [Scripts](scripts.md): lifecycle hooks, custom scripts and `stems run`, stamps, `soft_fail`, builds.
- [Running commands in a stem's context](exec.md): `stems exec`, `stems shell`, `stems env`.
- [Overlays](overlays.md): files rendered into codebases, `stems overlays render`.

## Containers

- [Docker stems](docker.md): images, registry credentials, builds and build secrets, `stems pull`.
- [Compose stems](compose.md): builds, stale images, env and `inject_env`, `stems compose`.

## Doctor & repos

- [Doctor](doctor.md): checks, `requires:`, custom `checks:`, `--fix`, orphans.
- [Git codebases](repos.md): clones, `stems repos sync` / `status` / `checkout`.

## Inspect

- [Logs](logs.md): records, levels, rotation, `stems logs`, export bundles, redaction.
- [Metrics](metrics.md): CPU, memory and disk per stem, thresholds, `stems metrics`.
- [Desktop notifications](notifications.md): when stems fail for good.

## Dashboard & agents

- [The dashboard (TUI)](tui.md): views, keys, actions, headless mode.
- [MCP server](mcp.md): tools, resources (`stems://features` among them), prompts, guard rails.
- [Local API protocol](protocol.md): the daemon's JSON-RPC socket and its events.
- [Command-line reference](cli.md): every command's `--help` (generated).

See also: [README](../README.md) · [Examples](../examples/README.md)
