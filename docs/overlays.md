# Overlays

An **overlay** is a file the integration repo materialises into a stem's
codebase while the stem runs — a `config/local.ini`, an `.env.local` — without
ever committing it there (FR-WS-7). stems writes it before the stem starts,
records it, and removes it again when the stem stops. Lifecycle order:
[scripts.md](scripts.md); state file: [recovery.md](recovery.md).

```yaml
stems:
  shop-api:
    codebase: ../../repos/shop-api
    env:
      SHOP_CONFIG: "${codebase}/config/local.ini"
    overlays:
      - { template: overlays/shop-api/local.ini.tmpl, dest: config/local.ini }
      - { file: overlays/shop-api/ca.pem, dest: certs/ca.pem, keep: true, mode: 0o600 }
```

| Field | Meaning |
|---|---|
| `template` | a file in the integration repo, rendered with `${…}` substitution |
| `file` | a file in the integration repo, copied byte for byte |
| `dest` | where it goes, **relative to the codebase**; absolute paths and `..` are refused (`SCHEMA_INVALID`) |
| `keep` | default `false`; `true` leaves the file in place on `down` |
| `mode` | file mode, default `0o644` |

Exactly one of `template` / `file`. Sources must stay inside the integration
repo (symlinks are resolved first).

## Templates

A template uses the same substitution as `stems.yaml`: `${var.x}`,
`${env.X}`, `${workspace.root}`, `${workspace.name}`, `${codebase}`,
`${stem.<name>.port}`, `${stem.<name>.ports.<p>}` and `${stem.self.port}`
(the stem's own primary port, so a template need not know its stem's name).
Ports are the ones the stem will run with: fixed ports as configured
(including `stems.local.yaml` overrides), `port: auto` ports as allocated
(the stem's own and its dependencies' are allocated if needed). `$${…}`
escapes a literal `${…}`; a `${…}` whose content cannot be a reference (such
as `${stem.<name>.port}` in a comment) is kept literally. An unresolvable
reference is `UNRESOLVED_VARIABLE` and fails the stem's start.

## When

```text
[setup]          (only when its stamp changed)
overlays         render → write, each recorded in state.json first
[pre_start]
start …
…
stop: [pre_stop] → stop → [post_stop] → overlays cleaned
```

Events: `overlay.materialised {dest, keep, backup}` per file written;
on stop `overlay.removed {dest}`, `overlay.kept {dest}` or the warning
`overlay.modified_left_in_place {dest}`.

## The "never touch code repos" guarantee

stems writes into a codebase **only** the `dest` of a declared overlay, and
only in these steps:

1. **Declared.** Nothing but the `dest` paths listed under a stem's
   `overlays:` is ever written, and a `dest` cannot leave the codebase
   (relative, no `..`). Scripts are the user's code and run with the
   codebase as cwd; what they do is theirs.
2. **Recorded before written.** Before a file is written, the record
   `{dest, sha256 of the bytes about to be written, run_id, keep}` is saved
   atomically in `state.json` (`overlays.<stem>`). The file itself is written
   atomically (temp file in the same directory, `fsync`, rename). A crash
   between the two leaves a record of a file that is missing, never a file
   stems does not know about.
3. **Never clobbers.** A `dest` that already exists is overwritten only if
   stems owns it — a record exists **and** the file still hashes to the
   recorded sha256. Anything else is `OVERLAY_CONFLICT`: the stem fails
   (`up` exits 1, or 3 when other stems came up) and the file is untouched.
   The one exception is `stems up --force-overlays`, which first copies the
   file to `$STEMS_STATE_DIR/overlay-backups/<run_id>/<absolute dest path>`
   (i.e. `<STEMS_HOME>/<ws-hash>/stems/<stem>/overlay-backups/…`, reported in
   `overlay.materialised`'s `data.backup`) and then overwrites it.
4. **Removed only if unchanged.** On `stop`/`down` a record is removed
   together with its file only when the file still hashes to what stems
   wrote. A file that was edited is left exactly as it is (and forgotten:
   it is yours now), with the warning event
   `overlay.modified_left_in_place`. `keep: true` files are left and stay
   recorded, so the next `up` reuses them instead of reporting a conflict.

Limits, stated plainly: directories stems had to create for a `dest` (e.g.
`config/`) are left behind, empty, on removal; and an overlay whose `dest` is
tracked by git dirties the repo while it exists (hence the warning below).

## Conflicts and warnings at validate time

`stems validate` (daemonless) reports `OVERLAY_CONFLICT` (exit 2) for every
`dest` that exists and is not in the state file's overlay ledger — so a file
stems wrote earlier (a running stem's overlay, a `keep: true` one) is not a
conflict. The daemon does not repeat this check when loading the workspace:
it decides per file at start time, by hash and honouring `--force-overlays`.

`validate` also warns `OVERLAY_TRACKED_FILE` for every `dest` tracked in the
codebase's git index (`git ls-files --error-unmatch`): materialising and
removing it would show up as changes in the repo. Warnings are listed in
`data.warnings` (same shape as errors) and never change the exit code. Fix:
add the path to the codebase's `.gitignore` and `git rm --cached` it, or
choose another `dest`. Hint for conflicts: *use `keep: true` and an explicit
`.gitignore`, or choose another `dest`*.

## `stems overlays [stem]`

Lists the recorded overlays and what is on disk now:

```json
{ "overlays": [ { "stem": "shop-api", "dest": "/…/shop-api/config/local.ini",
                  "status": "present", "keep": false, "sha256": "…", "run_id": "…" } ] }
```

`status` is `present` (the bytes stems wrote), `modified` (edited since;
`down` will leave it) or `missing`. It asks the daemon (RPC `overlays`
`{stem?}` → `OverlaysResult {overlays}`) when one runs and reads
`state.json` otherwise. An unknown stem is `UNKNOWN_STEM`.

## Crashes

The ledger lives in `state.json`, independent of whether the stem runs, and
is carried from daemon run to daemon run. After `kill -9` of the daemon,
`stems down` starts a new daemon that adopts the still-running stems and
stops them, which cleans their overlays as usual. Records of stems that did
not survive the crash are cleaned (same rules) when the new daemon starts.
