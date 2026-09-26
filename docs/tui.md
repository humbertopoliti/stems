# The dashboard (TUI)

`stems up` (attached) and `stems attach` open a terminal dashboard driven by
the daemon's RPCs and event stream (deliverables 27 to 30; FR-UI-1..4,
FR-GR-4/5/6, FR-LC-4, FR-LG-3/4, FR-SC-2, FR-WD-2, FR-AI-4).
The code lives in the `stems-tui` crate (ratatui 0.30 + crossterm 0.29).

## Opening it

| Command | What you get |
|---|---|
| `stems up` on a terminal | `up` progress, then the dashboard; quitting asks "Stop everything? [y/N/d(etach)]" |
| `stems attach` on a terminal | the dashboard on a running daemon; `q` just leaves (stems keep running) |
| `stems attach --view detail` | opens on the selected stem's detail (`--view table`, `--view graph` likewise) |
| `STEMS_TUI=0 stems up` / `stems attach` | the plain event stream (CI logs), as before 27 |
| stdout not a terminal, or `--json` | the plain event stream / NDJSON (unchanged) |
| `STEMS_TUI_FORCE=1` | the terminal dashboard even when stdout is not a terminal (no raw mode, no key input then; for the panic-restore test) |
| `stems attach --headless --script '...'` | replay keys, print frames as text (below) |

Quitting an attached `stems up`: `q` or `Ctrl-C` opens the confirmation;
`y` runs `down --all` (every stem and the daemon stop, as with Ctrl-C in
the plain stream), `d` detaches (the daemon keeps running; `stems attach`
reconnects), `n`/`Esc`/`Enter` cancel. `SIGTERM`/`SIGHUP` (terminal closed)
stop everything in attached `up` and just leave in `attach`. A plain
`stems attach` quits at once on `q`/`Ctrl-C`.

## Layout

```
 stems · minimal                            Graph [Table] Detail  Logs  Events
  STEM     TYPE    STATUS     REASON   PID    PORTS  UPTIME RESTARTS CPU   MEM
› echo-svc process ✓ healthy  -        12345  18090  42s    0

 minimal · profile - · daemon pid 4242 · ✓1 !0 ✗0 ·0 ?0 ↻0     ? help · q quit
```

* Title bar: workspace and the views; the current one is in brackets.
* Body: the current view.
* Status bar: workspace, profile, daemon pid, counts per glyph (healthy,
  degraded, failed, stopped, unknown, transitioning), the filter (`/b`),
  then key hints or the last notice/error.

### Views

* **Table**: the `stems status` columns plus CPU and MEM sparklines (empty
  until deliverable 25 feeds samples). Columns adapt to the width (below
  100 columns PORTS shows only port numbers); names and reasons truncate
  with `…`. The selected row is marked `›` (`>` in ASCII).
* **Detail**: the selected stem's effective config (the `stem_config` RPC:
  the resolved stem as the daemon loaded it, shown as YAML), its state,
  pid, uptime, restarts and ports, the scripts, health (the probe summary
  from `status` and the last probe results from the `health` RPC, or
  `n/a`), metrics (placeholder until 25) and the last 10 events of the stem.
* **Graph**: the dependency graph with live glyphs (below). It is the
  default view when the workspace has more than one stem; with one stem
  the dashboard opens on the Table (`ui.default_view` or `--view` win).
* **Logs**: the selected stem's logs, or every stem merged (`m`), live
  (below).
* **Events**: the daemon's event stream as a table (below).

`Tab` cycles Graph → Table → Detail → Logs → Events.

Terminals smaller than 40x10 show "terminal too small" instead.

With the **split layout** (`Ctrl-L`) the bottom 40 % of Table, Graph and
Detail is the log pane of the selected stem, following; it moves with the
selection. It is the same component as the Logs view. `Ctrl-L` saves the
choice as `split_logs` in `ui.toml` (the one line is replaced or added,
the rest of the file is kept), so the next session starts split.

### The graph view (28)

```
 stems · process-chain                     [Graph] Table  Detail  Logs  Events
             ┌─────┐
         ╭──▶│ b ✓ ├──╮
┌─────┐  │   └─────┘  │   ┌─────┐
│ d · ├──┤            ├──▶│ a ✗ │
└─────┘  │   ┌─────┐  │   └─────┘
         ╰──▶│ c ↻ ├──╯
             └─────┘
 ✓ healthy  ! degraded  ✗ failed  · stopped  ? unknown  ↻ transitioning
```

* The same layered layout as `stems graph` (`stems_core::layout`):
  dependants on the left, arrows to their dependencies on the right, soft
  edges dashed (`╌`, dimmed). The layout is built from the daemon's
  `stem_config` of every stem and cached on the set of stem names: it is
  rebuilt only when that set changes, while glyphs follow every `status`.
* Each box is `name glyph`; a failed or degraded stem gets its reason on a
  second line (`dep shop-api` for a stem whose hard dependency is unhealthy
  or failed, FR-GR-6; other reasons as `status` gives them, cut at 28
  characters).
* The selected box is in reverse video. The graph starts on its first box
  (top of the leftmost column) until you move.
* Zoom: by default boxes are compact (`[name ✓]`, one row, no reason line)
  only when the full drawing does not fit the view; `-` forces compact
  boxes, `+` full boxes, `0` back to auto. A drawing larger than the view
  scrolls to keep the selected box visible.
* The last line is the legend, with the active modes on the right when
  there is room (`focus web · labels · zoom auto`).

### The Logs view (29)

```
┌ Logs: echo-svc · following · level warn+ ────────────────────────────────────┐
│   12:00:01.042 WARN chaos endpoints enabled under /__chaos/                  │
│ * 12:00:03.311 ERROR chaos log line 1                                        │
│›  12:00:03.312 INFO  GET /users 200 ▾ request_id=65e8327232d0                │
│      request_id: "65e8327232d0"                                              │
│ /line 1 · 1 match                                                            │
└──────────────────────────────────────────────────────────────────────────────┘
```

* Backed by `subscribe_logs` for the selected stem (or every stem with
  `m`, each line prefixed with its stem in a stable colour, as in
  `stems logs`): the last 2000 lines are replayed, then new ones arrive
  live. The pane keeps a ring of 20 000 lines (the oldest go) with a
  search key per line (the text and the fields, lowercased); a search over
  the full ring takes a few milliseconds (budget 50 ms).
* The selected line is marked `›` (`>` in ASCII) and shown in reverse; it
  follows the newest line until you move up (the title says `following`
  or `scrolled`); `G` follows again. `Space` pauses: new lines wait and
  the title says `paused (+N)`; `Space` again shows them and follows.
* `/` searches (case-insensitive, while typing; `Enter` keeps it and goes
  to the newest match at or above the selection, `Esc` clears it): the
  matches are highlighted and marked `*` in the gutter, the bottom bar
  shows `/text · N matches · i/N`; `n`/`N` go to the next/previous match
  (wrapping).
* `L` cycles the level filter `all → info+ → warn+ → error` (lines
  without a detected level show only with `all`); `s` hides/shows script
  output lines (`[script]` prefix); `w` wraps long lines; `t` hides/shows
  the timestamps (UTC, like the detail view's events).
* Structured (JSON) lines (12) show the level (coloured), the message and
  the other fields collapsed (`▸ k=v ...`); `Enter` expands them, one per
  line (`key: value`), and again collapses them. Plain lines are coloured
  by their detected level (error red, warn yellow, debug/trace grey).
* `y` copies the selected line's text; `V` starts a visual range (marked
  `│`), move, then `y` copies the range (one line per record). See
  [Clipboard](#clipboard).
* `[` / `]` switch to the previous / next stem.

### The Events view (29)

```
┌ Events · /failed ────────────────────────────────────────────────────────────┐
│  TIME     KIND            STEM       FROM→TO             REASON      ACTOR   │
│› 12:00:03 stem.state      echo-svc   healthy→failed      exit code 1 daemon  │
└──────────────────────────────────────────────────────────────────────────────┘
```

* The daemon's events (the buffered ones, fetched with `events` when the
  view first opens, then live ones; the last 2000 are kept), oldest first,
  the newest selected until you move.
* `/` filters by a substring of the stem, kind, from/to state or reason.
* `Enter` opens the event's stem in the Logs view replayed from a minute
  before the event (`subscribe_logs` with `since`), the selection on the
  last line at (or up to 2 s after) the event time, not following.

### Clipboard

`y` never blocks. By default it writes an OSC 52 sequence
(`ESC ] 52 ; c ; <base64> BEL`) to the terminal, which works over ssh and
in tmux (with `set-clipboard on`) when the terminal allows it. With
`clipboard = "command"` in `ui.toml` the text is piped into `pbcopy`
(macOS), `wl-copy` (Wayland) or `xclip -selection clipboard`, on a
background thread. Headless mode prints the OSC 52 sequence to stdout on
its own line (and never runs a clipboard command). The status bar says
`copied N lines`.

## Keys

| Key | Action |
|---|---|
| `j` / `k`, `↓` / `↑` | move the selection (graph: within a column) |
| `h` / `l`, `←` / `→` | graph: move to the column on the left / right (the first connected stem there, else the same position) |
| `f` | graph: focus mode, only the selected stem and its direct neighbours (again to leave) |
| `e` | graph: edge labels from `protocol` / `via` (FR-GR-5) |
| `+` / `-` / `0` | graph: full boxes / compact boxes / auto |
| `g` / `G`, `Home` / `End` | first / last stem |
| `Enter` | open the detail view (`Esc` goes back to the graph or table) |
| `Tab` / `Shift-Tab` | next / previous view |
| `/` | filter by name: type, `Enter` keeps it, `Esc` clears it |
| `O` | cycle the sort: config order, name, state (failed first), uptime (was `s` before 30) |
| `Esc` | dismiss the toasts, close the help / dialog, back from Detail, clear the filter |
| `?` | toggle the help overlay |
| `q`, `Ctrl-C` | quit (see above) |
| `Ctrl-L` | split layout: the selected stem's log pane under Table/Graph/Detail (saved in `ui.toml`) |
| `Ctrl-P` | the command palette (any view; below) |
| mouse click | select a table row (`mouse = true`) |

In the Logs view:

| Key | Action |
|---|---|
| `Space` | pause / resume following |
| `/`, `n` / `N` | search; next / previous match |
| `L` | level filter: all, info+, warn+, error |
| `s` | show / hide script output lines |
| `m` | merged: every stem, with stem prefixes |
| `[` / `]` | previous / next stem |
| `j` / `k`, `PgUp` / `PgDn` | move the selected line |
| `g` / `G`, `Home` / `End` | top / bottom (bottom follows) |
| `Enter` | expand / collapse a structured line's fields |
| `V`, `y` | start / leave a visual range; copy the line or the range |
| `w`, `t` | wrap long lines; timestamps |
| `Esc` | leave the range, clear the search |

In the Events view: `j`/`k`, `g`/`G` move, `/` filters, `Enter` jumps to
the logs, `Esc` clears the filter. `?` shows the keys of the current view.

## Actions (30, FR-UI-2)

In Table, Graph and Detail these keys act on the selected stem. Each one
is one RPC on the dashboard's connection, whose requests carry
`actor: tui:<user>` (FR-AI-4): the events they cause (`stem.state`,
`watch.paused`, `script.started`, ...) say the TUI did it. While the call
runs the status bar says `restart shop-api…`; the result is a toast.

| Key | Action | RPC |
|---|---|---|
| `s` | start (and its dependencies) | `start {stems: [stem]}` |
| `x` | stop; when running stems depend on it the daemon answers `HAS_DEPENDANTS` (nothing stops) and a dialog names them: `y`/`X` stop them too, `n`/`Esc`/`Enter` cancel | `stop {stems, cascade: false}` |
| `X` | stop with its running dependants, no question | `stop {stems, cascade: true}` |
| `r` | restart (keeps ports) | `restart {stems}` |
| `R` | rebuild + restart (`build` script first) | `restart {stems, build: true}` |
| `p` | pause the stem's watchdog, or resume it when paused; paused stems show `⏸` (`\|\|` in ASCII) after their name in the table and on their graph box | `watch_pause` / `watch_resume {stems}` |
| `o` | open the stem's codebase (else the workspace root) in `$EDITOR` (else `$VISUAL`, else `vi`); the dashboard suspends (leaves the alternate screen and raw mode, stops reading keys), runs `sh -c '$EDITOR "$1"' sh <dir>` on the terminal and comes back; a toast says how it ended | `stem_config` for the path |
| `S` | reset: a dialog asks to type `reset` then `Enter` (anything else clears the field, `Esc` cancels); stops the stem, runs its `reset` script, clears its stamps. The typed word is the confirmation (the RPC has no `yes` flag; the CLI's `--yes` guards only `down --volumes`) | `reset {stems: [stem]}` |
| `u` | up everything (the session's profile, if any) | `up {profile}` |
| `d` | down everything, after a `[y/N]` dialog: in `stems attach` it runs `down --all` (the daemon stops, the dashboard closes); in attached `stems up` it is the same as `q` then `y` | `down {all: true}` |
| `:` | the script menu (below) | `script_catalog`, `run_script` |

### The script menu (`:`, FR-SC-2)

Lists the selected stem's scripts from `script_catalog` (lifecycle ones
first, then custom ones, each by name, with their descriptions), then the
workspace's scripts in a second section. Typing filters (fuzzy, as in the
palette), `↑`/`↓` (or `Tab`, `Ctrl-N`/`Ctrl-P`) move, `Enter` runs,
`Esc` clears the filter, then closes. A `…` after a name means the script
declares `args`: `Enter` opens its form:

* one field per argument, in declaration order, defaults prefilled; `*`
  marks required ones; the description shows under the field;
* text fields (`string`, `int`, `float`, `path`) take typed characters
  (`Backspace`, `Ctrl-U` clears); an `enum` is a `‹ admin ›` picker
  (`←`/`→`, also `h`/`l`; an optional enum without default includes "not
  given"); a `bool` is a toggle (`Space`, `←`/`→`);
* `Tab`/`↓` and `Shift-Tab`/`↑` move between fields;
* `Enter` validates with the daemon's own rules
  (`stems_core::scriptargs::parse_args_json`): an error (a required
  argument left empty, `rows=many` for an `int`) shows inline under the
  field, focus moves there and **nothing is sent**; when valid the script
  runs with `run_script {stem, name, args: {...}, wait: false}`.

A running script opens the split log pane (`Ctrl-L` closes it) subscribed
to the script's stem, so its output lines, tagged `[create-test-user]`,
stream in; a workspace script's output (log stem `_workspace`) is shown
there until the selection moves. When its `script.finished` event arrives
(matched by `run_id`) a toast says `✓ create-test-user finished in 1.2s`
or `✗ create-test-user failed: exit 3` (`timed out`, `signal N`).

### The command palette (`Ctrl-P`)

A fuzzy search over everything the dashboard can do: per stem `restart
shop-api`, `start …`, `stop …`, `stop … (cascade)`, `rebuild …`, `pause
watch …` / `resume watch …`, `open … in $EDITOR`, `reset …`, `scripts of
…`, `view logs …`, `view detail …`; every script of the catalogue (`run
seed-large (api)`, `run needs-api (workspace)`); `view table|graph|…`,
`up all`, `down all`, `help`. Type (`rest api`), `↑`/`↓` move, `Enter`
selects the stem and does exactly what its key would (dialogs included),
`Esc` closes.

The matcher (`stems_tui::fuzzy`) is small and deterministic: each query
word must be a case-insensitive subsequence of the label, the words in
order; per matched character `+1`, `+5` when it directly follows the
previous match, `+3` at a word start (after a space, `-`, `_`, `.`, `/`,
`(` or `:`), `-1` per skipped character (at most 5 per gap, also before a
word's first match); every start position of a word is tried. Ties go to
the shorter label, then the list order. So `rest api` ranks `restart
shop-api` above `reset shop-api`.

### Toasts

Results and errors stack bottom-right above the status bar (newest
lowest, at most four), each for 5 s of the tick clock (`refresh_ms` per
tick; headless runs have no ticks, so their toasts stay until dismissed).
`Esc` dismisses them all. A success reads `✓ restarted shop-api`; an error
`✗ HAS_DEPENDANTS stop a: cannot stop a: …` with its hint on the next
line; `e` opens the newest error's details (code, message, hint, the
`details` JSON) in a dialog (while an error toast shows, `e` does this
instead of toggling the graph's edge labels).

When the daemon reports `config.changed` (33: the config on disk differs
from the applied one) a toast `• config changed: N stems affected` stays
until acted on: `a` applies it (`config_apply {yes: true}`), `v` shows the
plan (`config_diff`: one line per affected stem with its action and the
changed fields, then the workspace-level changes; `a` applies from there
too, `Esc` closes). `config.applied` removes the toast.

### Dialogs

Every dialog is a `Modal` variant and takes every key while open
(`update::modal_key`): `QuitConfirm` (27), `StopConfirm {stem,
dependants}`, `DownConfirm`, `ResetTyped {stem, buffer}`, `ScriptMenu`,
`ScriptForm`, `Palette`, `ErrorDetails {title, text}` and `Plan`.
`Ctrl-C` closes any of them.

## Preferences: `~/.config/stems/ui.toml` (FR-UI-4)

```toml
theme = "dark"          # dark | light (accent colours)
mouse = false           # clicking a table row selects it
default_view = "graph"  # table | detail | graph | logs | events (unset: graph for >1 stem, else table)
refresh_ms = 250        # tick interval, 50..5000; status refreshes about every second
split_logs = false      # the split log pane (Ctrl-L toggles and saves it)
clipboard = "osc52"     # osc52 | command (pbcopy / wl-copy / xclip)
```

Path: `$STEMS_UI_CONFIG`, else `$XDG_CONFIG_HOME/stems/ui.toml`, else
`~/.config/stems/ui.toml`. A missing file means the defaults; an unreadable
or invalid one falls back to the defaults and the status bar says why.
`--view` beats `default_view`. Headless runs read only `$STEMS_UI_CONFIG`,
so a developer's own file never changes test frames.

## Headless mode (tests, CI, bug reports)

```sh
stems attach --headless --script 'wait:healthy;frame;j;Enter;frame' [--size 120x40] [--frames-out DIR] [--json]
```

Keys are replayed against the live daemon with ratatui's `TestBackend`
(default 80x24). Each `frame` token prints the frame as text after a
`--- frame N ---` line (with `--frames-out DIR` also `DIR/frame-NNN.txt`);
with an explicit `--json` nothing is printed while running and the envelope
is `{detached: true, frames: [...]}`. `--script @FILE` reads the tokens from
a file (one or more per line, `#` comments). Before each `frame` the status
is refreshed, so the frame shows the daemon's current state.

| Token | Effect |
|---|---|
| `j`, `q`, `?` (one character) | that key |
| `Enter`, `Esc`, `Tab`, `BackTab`, `Up`, `Down`, `Left`, `Right`, `Home`, `End`, `Backspace`, `Space`, `Ctrl-C`, `Ctrl-L`, `Ctrl-P` (any `Ctrl-<char>`) | a named key |
| `/api<Enter>` (anything else) | typed character by character; `<Name>` inside is a named key |
| `wait:<state>` | wait until every stem is in `<state>` (a state such as `healthy`/`failed`, or a glyph name); fails after 30 s |
| `wait:stem=<name>:<state>` | wait for one stem |
| `wait:lines>=<n>` | wait until the log pane (Logs view or split pane) holds `n` lines (paused ones included) |
| `wait:log=<text>` | wait until a log pane line contains `<text>` |
| `wait:event=<kind>[:<stem>]` | wait until an event of that kind (and stem) arrives after the last key token (30), e.g. `wait:event=script.finished:shop-api` |
| `type:<text>` | the characters of `<text>` as keys, verbatim (spaces, `<`, `>` included): typing into the focused field, filter or palette (30), e.g. `type:rest api` |
| `chaos:<path>` | `GET http://127.0.0.1:<port>/__chaos/<path>` on the selected stem's first port (the shop-api chaos endpoints of the test workspaces; errors become a notice) |
| `view:<name>` | switch view |
| `frame` | dump the frame |
| `sleep:<ms>` | pause (scripts only; scenarios use waits) |

Waits are event-driven (the daemon's `stem.state` events patch the table)
with a status poll every 250 ms as a fallback. The script's end detaches
(exit 0). Frames use Unicode glyphs unless `STEMS_ASCII=1`.

Actions run synchronously in headless mode: after `r` the restart has
finished (its toast is in the next frame), so a scenario needs no wait for
it. `o` honours `EDITOR` too: the editor runs with no terminal (stdio to
`/dev/null`) and the frame after it is the normal dashboard, e.g.
`EDITOR=fake-editor stems attach --headless --script 'wait:healthy;o;frame'`
with a fake editor that records its argument
(`tests/features/tui/open-editor.feature`).

Attached `stems up` has no flags for this; the environment drives it:
`STEMS_TUI_SCRIPT` (the script; enables headless mode), `STEMS_TUI_SIZE`
(`WxH`) and `STEMS_TUI_FRAMES_OUT` (a directory). E.g. the quit
confirmation scenario runs `STEMS_TUI_SCRIPT='wait:healthy;q;y' stems up`.

## Terminal hygiene

The dashboard enters the alternate screen and raw mode (only when both
stdin and stdout are terminals; a background process group changing
terminal modes would be stopped by `SIGTTOU`). They are restored on normal
exit (a guard), on `SIGTERM`/`SIGHUP` (turned into an orderly exit) and on
panic: a panic hook (installed once, wrapping the previous hook) writes the
restore sequence (`ESC[?1049l`, cursor shown, mouse capture off) before the
panic message. `STEMS_TUI_PANIC_TEST=1` panics after the first frame to
prove it (`tests/features/tui/panic-restore.feature`, run with
`STEMS_TUI_FORCE=1` on a pipe).

## For developers

* `stems_tui::{Model, Msg, Cmd, update, view}`: Elm-style. `update(&mut
  Model, Msg) -> Vec<Cmd>` is pure; `view(&Model, &mut Frame)` is pure;
  `runner::{run_terminal, run_headless, exec}` execute `Cmd`s (`status`,
  `daemon_status`, `stem_config`, `events`, `health`) and feed results back
  as `Msg::Status` / `Msg::Rpc`.
* Logs (29): `stems_tui::logs::LogPane` is a self-contained component
  (`push(LogRecord)`, `key(KeyEvent) -> PaneAction`, `render(frame, area,
  &PaneStyle)`) over a `LogRing` (20k lines, `search`); the Logs view and
  the split pane share `Model::log_pane`, and 30's script output can own
  another one. The reducer keeps the pane subscribed to what is on screen
  (`update::log_target`): `Cmd::SubscribeLogs(LogSubscription)` with a
  generation number; the runners abort the previous subscription and feed
  `Msg::Log { generation, record }` (older generations are dropped).
  `Cmd::Copy` and `Cmd::SavePref` are executed by the runners too;
  `Cmd::LoadEvents` fills the Events view. Headless output is
  `run_headless_output` (`HeadlessOutput::{Frame, Raw}`).
* Actions (30): `stems_tui::actions::Action` is the dispatcher, one
  variant per RPC (`method()`, `params()`, `describe()`, `done(result)`);
  `Cmd::Action(Action)` is executed by `runner::exec` (10 min timeout) and
  comes back as `RpcResult::Action {action, result}`, which the reducer
  turns into a toast (or the stop dialog on `HAS_DEPENDANTS`) and a status
  refresh. `Cmd::LoadCatalog` fills `Model::catalog` (the script menu and
  the palette), `Cmd::LoadPlan` the plan dialog, `Cmd::OpenEditor` is run
  by the runners (the terminal one suspends the dashboard). The MCP server
  (31) has its own dispatcher over the same RPCs. The form is
  `stems_tui::form::ScriptForm` (`key(KeyEvent) -> FormAction`), toasts
  are `stems_tui::toast::Toasts` (`push`, `expire(now)`, `dismiss`), the
  dialogs and toasts are drawn by `stems_tui::modals`. Goldens of every
  dialog at 80x24: `src/tests/snapshots/`.
* The graph: `stems_tui::graph::GraphWidget` renders any `Layout` into a
  ratatui buffer (builder: `.status(..)`, `.selected(..)`, `.unicode(..)`,
  `.edge_labels(..)`, `.compact(..)`), reusable e.g. as a mini-map. It
  draws `stems_core::render::render_grid`: the text renderer's cells plus,
  per cell, the box it belongs to, its glyph colour and whether it is a
  (soft) edge. `Cmd::LoadGraph` / `RpcResult::Graph` load it;
  `graph::step` holds the selection rules.
* Frame goldens: `stems_tui::assert_frame!(model, "name")` snapshots the
  frame at 80x24 and 120x40 (`crates/stems-tui/src/snapshots/`).
* E2E: `Then the frame matches golden "<name>" masking PID,UPTIME,CPU,MEM`
  (goldens in `tests/features/goldens/`; `TIME` masks clock times such as
  log timestamps), `the last frame contains`, `frame N contains`,
  `stdout contains the OSC 52 sequence for "<text>"` (see
  `tests/stems-e2e/STEPS.md`).
