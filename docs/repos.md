# Git codebases and `stems repos` (deliverable 20)

A stem's `codebase` is normally a local path. It can also be a git
repository that stems clones and keeps at a ref for you (FR-WS-3):

```yaml
stems:
  shop-api:
    type: process
    codebase:
      git: git@github.com:acme/shop-api.git   # https://, ssh://, git://, file:// or user@host:path
      ref: main                               # branch, tag or commit sha; omit for the default branch
      # path: ../checkouts/shop-api           # optional: clone here instead
    scripts: { start: python3 app.py }
```

A bare git URL string (`codebase: git@github.com:acme/shop-api.git`) is the
same as `{ git: <url> }` without a ref.

stems runs the **`git` CLI** (never a library), so your credentials, SSH
config, credential helpers and `insteadOf` rules apply unchanged. git is run
with `GIT_TERMINAL_PROMPT=0`: a repository that needs an interactive password
fails instead of hanging the daemon.

## Where clones live

`<workspace>/.stems/repos/<stem>` by default. `stems init` gitignores
`.stems/`. Change the directory for every stem with the workspace-level
`repos_dir:` (relative to `stems.yaml`), or for one stem with `codebase.path`.

## When stems clones and fetches

| Command | Missing clone | Existing clone |
|---|---|---|
| `stems up` / `start` / `restart` | cloned before the stem's `setup` | **never fetched** |
| `stems up --sync` | cloned | fetched + ref checked out (see below) before anything starts |
| `stems repos sync [stems…]` | cloned | fetched + ref checked out |

Cloning: `git clone --branch <ref> <url> <dir>` for a branch or tag;
`git clone <url> <dir>` then `git checkout --detach <sha>` for a sha (7–40 hex
digits). `--recurse-submodules` is passed through with `stems repos sync
--recurse-submodules`. A clone that fails leaves no directory behind.

Updating an existing clone (`repos sync`, `up --sync`) — **stems never
touches developer work**. The clone is left exactly as it is and reported
`skipped_dirty` (event `repo.skipped_dirty`, with `reason`) when:

* the working tree has changes (`git status --porcelain` is not empty) —
  `uncommitted_changes`;
* HEAD is no longer where stems put it: you switched to another branch, or
  from a detached tag to a branch (stems records what it checked out in the
  clone's git dir, `stems-sync`) — `moved`;
* HEAD has commits that are on no remote branch or tag — `unpushed_commits`.

Otherwise stems runs `git fetch --tags --force --prune origin` and checks out
the ref: a branch is checked out (created tracking `origin/<ref>` if needed)
and **fast-forwarded only**; a tag or sha is checked out detached; without a
ref the current branch is fast-forwarded to its upstream.

With `up --sync`, a failed fetch is not fatal: it is reported as a
`repo.failed` event and the stem starts from the checkout it has. A failed
clone (a missing directory) fails the stem.

## Local overrides

Already have the repository checked out? Point the stem at it in
`stems.local.yaml`; the path **replaces** the git form entirely (nothing is
cloned, `repos sync` reports `skipped_local`, `repos status` shows `source:
local`):

```yaml
stems:
  shop-api:
    codebase: ~/work/shop-api
```

You can also keep the git form and only change a key, e.g.
`stems: { shop-api: { codebase: { ref: my-branch } } }`.

## `stems repos sync [stems…] [--json]`

Default: every enabled stem with a codebase. `data`:

```json
{ "ok": true,
  "repos": [ { "stem": "shop-api", "path": "/ws/.stems/repos/shop-api",
               "action": "checked_out", "ref": "v2",
               "sha": "3f2c…", "message": "checked out `v2`" } ] }
```

`action` is one of `cloned`, `fetched` (the branch fast-forwarded),
`checked_out` (switched to the ref), `up_to_date`, `skipped_dirty`,
`skipped_local`, `failed`. A failed entry carries its `error`, which is also
in the envelope's `errors` (exit 1).

## `stems repos status [stems…] [--json]`

Per stem: `stem`, `source` (`git` | `local`), `path`, `url`, `ref`
(configured), `branch` (current; `null` when detached), `sha` (HEAD),
`dirty`, `ahead` / `behind` (against the branch's upstream; `null` without
one) and `exists`. Local codebases that are git checkouts get the same git
fields. Nothing is fetched.

Both commands run inside the daemon when one runs for the workspace (so
`repo.*` events are emitted and git's output is logged), and in-process
otherwise.

## Logs and events

The output of the git commands that change something (`clone`, `fetch`,
`checkout`, `merge`) goes to the stem's log as `stream: script, tag: git`:
`stems logs shop-api --script git`.

| Event | `data` |
|---|---|
| `repo.cloned` | `url, ref, sha, path` |
| `repo.fetched` | `url, ref, sha, updated` |
| `repo.checked_out` | `url, ref, sha, from, path` |
| `repo.skipped_dirty` | `url, ref, sha, path, reason` |
| `repo.failed` | `url, ref, path, error` |

## Errors

| Code | When | Exit |
|---|---|---|
| `GIT_CLONE_FAILED` | clone, fetch or checkout failed; `details: { stem, url, ref, tail }` (last lines of git's stderr) | 1 |
| `GIT_NOT_INSTALLED` | `git` is not on `PATH` | 1 |
| `CODEBASE_NOT_FOUND` | a *local* codebase path does not exist (`validate`) | 2 |
| `SCHEMA_INVALID` | `codebase.git` is not a URL (`validate`), path `stems.<s>.codebase.git` | 2 |

## RPCs

`repos_sync` (`{stems, force_fetch = true, recurse_submodules}` →
`{ok, repos}`) and `repos_status` (`{stems}` → `{repos}`); `up` takes
`sync: bool`. Library: `stems_daemon::repos::{sync, status, ensure_clone}`.
