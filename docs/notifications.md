# Desktop notifications

When a stem ends up **`failed`** and nothing in stems will retry it any more,
the daemon posts **one** desktop notification saying what failed and why,
however many stems failed at once. It is meant to be glanced at when `stems
up` runs in a detached terminal or a dashboard is not on screen; everything
it says is also in `stems status` and the events stream.

```
stems · hello-shop
shop-api failed: was restarted 5 times within 10m and failed again: giving up (restart.max = 5)
check `stems logs shop-api`, fix the crash and run `stems restart shop-api`
```

## What triggers one

Every path that gives up on a stem ends in the `failed` state: the restart
budget (`MAX_RESTARTS`, [restart.md](restart.md)), `START_TIMEOUT`, a process
that exits before it is ready (`START_FAILED`), `restart.policy: never` after
a crash, a failed `setup`/`seed`/`build` script (`SETUP_FAILED`,
`SCRIPT_FAILED`), an overlay conflict. The notifier subscribes to the event
stream and reacts to `stem.state` with `to: failed`, so there is no list of
causes to keep in step. Policy restarts are not failures (they show as
`starting`, reason `restarting in …`).

Never a notification:

- a stem you stop (`stems stop`, `stems down`): `stopped` is not `failed`,
  and a failure *while* a stem is being stopped (`from: stopping`) or while a
  `down` is in flight is ignored;
- `unhealthy`, `degraded` or a metric threshold: those are not "gave up"
  states and would multiply the noise;
- the daemon shutting down.

## One notification, not twenty

Three layers coalesce failures into one notification per burst:

1. **Debounce** (`notifications.debounce`, default `2s`): the first failure
   opens a batch; it is posted after that much quiet, at the latest five
   debounces after the first failure however many keep arriving.
2. **Early flush**: when the command that caused the failures returns
   (`up.finished`, `cascade.finished`, `cascade.aborted`) an open batch is
   posted at once, so the notification lands as `stems up` prints its
   summary even with a long debounce.
3. **Cooldown** (`notifications.cooldown`, default `30s`, per workspace):
   after a notification, failures that arrive inside the cooldown are held
   and posted together when it ends. Twenty stems failing in a storm are one
   notification; a crash loop you keep restarting by hand posts at most two
   a minute. The held failures are listed like any other; the event counts
   them in `folded`.

Backends that support a group/replace id (`terminal-notifier -group`,
`notify-send -r`) reuse one id per workspace, so a later notification
replaces the earlier one in Notification Centre instead of stacking.

## What it says

- title: `stems · <workspace name>`;
- one stem: `<stem> failed: <error message>` (cut at about 120 characters),
  second line the error's hint (e.g. `stems logs shop-api`);
- two or three: `shop-api, worker, redis failed`, second line a tally per
  code (`2 × MAX_RESTARTS, 1 × START_TIMEOUT`);
- more: `12 stems failed: shop-api, worker, redis, +9 more` and the tally.

The body carries the error message only: never a command line, environment
or probe detail (they can hold secrets). A `failed` transition without an
error (policy `never` after an exit) is reported with the code `EXITED` and
the reason (`exited with code 3`).

## Backends

The first one found on the daemon's `PATH`, in this order:

| OS | backend | notes |
|---|---|---|
| macOS | `terminal-notifier` | if you installed it (`brew install terminal-notifier`); `-group` replaces the previous notification |
| macOS | `osascript` (`display notification`) | always there; the sender shows as "Script Editor" and a click opens nothing; notifications stack |
| Linux | `notify-send` (libnotify) | `-r` replaces (libnotify ≥ 0.7.9; older versions post without replacing) |
| any | `notifications.command` | yours; replaces the OS backend (see below) |

Nothing found: one `notification.skipped {reason: "no_backend"}` event per
daemon run and a line in the daemon log; later batches are silent. `stems
doctor` has a `notifications` check that says which backend would be used.

The daemon runs detached (`setsid`, cwd `/`) and `osascript` from such a
process still posts to the logged-in user's Notification Centre. A backend
that exits non-zero or takes more than 5 s is one `notification.skipped
{reason: "failed", error}` (daemon log at `warn`), never retried for that
batch.

### Your own command

```yaml
notifications:
  command: scripts/notify.sh      # /bin/sh -c, in the workspace root
```

The command runs with `/bin/sh -c` in the workspace root, with the
notification in its environment and the JSON payload on stdin:

| variable | value |
|---|---|
| `STEMS_NOTIFY_TITLE` | `stems · <workspace>` |
| `STEMS_NOTIFY_BODY` | the body |
| `STEMS_NOTIFY_SUBTITLE` | the second line (unset when there is none) |
| `STEMS_NOTIFY_STEMS` | comma-separated stem names |
| `STEMS_NOTIFY_CODES` | comma-separated error codes |
| `STEMS_NOTIFY_WORKSPACE` | the workspace name |
| `STEMS_NOTIFY_FOLDED` | failures held back by the cooldown |

stdin: `{"title", "body", "subtitle"?, "stems", "codes", "workspace",
"folded"}`. A Slack webhook, for instance:

```sh
#!/bin/sh
curl -sS -X POST -H 'content-type: application/json' \
  -d "{\"text\": \"$STEMS_NOTIFY_TITLE: $STEMS_NOTIFY_BODY\"}" "$SLACK_WEBHOOK_URL"
```

## Configuration

```yaml
notifications:
  enabled: true        # default true
  debounce: 2s
  cooldown: 30s
  command: null        # a custom notifier; null = the OS backend
```

Every key is optional and overridable in `stems.local.yaml`; the daemon
reads the applied config at every notification, so a change takes effect
with the config reload ([config.md](config.md#reload-on-change-fr-wd-3); a
`notifications:` change is a hot, workspace-level change,
`notifications_changed`).

**Kill switch**: `STEMS_NOTIFICATIONS=0` (or `off`, `false`) in the
daemon's environment disables notifications whatever the file says: for CI,
scripts, or a test harness (the e2e suite sets it for every command). The
failure is then `notification.skipped {reason: "disabled"}`.

## Events

| kind | data |
|---|---|
| `notification.sent` | `stems`, `codes`, `backend` (`terminal-notifier`, `osascript`, `notify-send`, `command`), `title`, `body`, `subtitle`, `replaced`, `folded` |
| `notification.skipped` | `reason` (`disabled`, `no_backend`, `failed`), `stems`, `codes`, `error` (for `failed`) |

Both carry actor `daemon`; `stems events`, the dashboard's events pane and
MCP clients see them like any other event.

## Not covered

- Suppressing notifications while a dashboard is attached (`when:
  unattended`): a later option; the default would stay "always".
- Clickable actions (open the logs): needs an app bundle, not `osascript`.
- Windows: stems is Unix-only.
