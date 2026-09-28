# Process model

How `stems_runtime::ProcessRuntime` starts, observes and stops native processes.
Code: `crates/stems-runtime/src/{process,os,output}.rs`.

## Sessions and process groups

Every stem is spawned with `setsid()` in `pre_exec`, so the leader is the leader
of a **new session and a new process group** whose id equals its pid
(`pgid == pid`). Everything it forks (npm → node → esbuild, a shell's background
jobs, shop-api's chaos children) stays in that group unless it calls `setsid`/
`setpgid` itself. stdin is `/dev/null`; stdout and stderr are pipes read by the
daemon — there is no controlling terminal, so nothing gets `SIGHUP`/`SIGTTOU`
from a tty and closing the user's terminal does not touch the stems.

Right after `spawn` the runtime records `pid`, `pgid` and the process
**start time** (before anything can reap the leader). A per-child task awaits
`Child::wait()`, which reaps the leader (no zombies) and publishes its
`ExitStatus { code, signal }`. Grandchildren that outlive the leader are
reparented to launchd/init, which reaps them.

## Stopping: always the group, never a pid

```
kill(-pgid, SIGTERM)
wait until leader exited AND no live process remains in pgid, at most `grace`
  -> all gone:      StopOutcome::Graceful
  -> otherwise:     kill(-pgid, SIGKILL), wait for the leader,
                    re-send SIGKILL to the group until it is empty (≤ 3 s sweep)
                    StopOutcome::Killed
nothing of ours running when stop was called -> StopOutcome::AlreadyDead
```

The runtime never calls `kill(pid)`: signalling only the leader would orphan
its children. `os::kill_group` refuses `pgid <= 1` and the daemon's own group, so a
corrupt state record cannot signal the daemon or every process of the user.
"Group is empty" means `kill(-pgid, 0)` fails *or* the group contains only
zombies (`os::process_tree` skips zombies).

## Start-time verification (pid-reuse defence)

Pids are recycled, so a pid alone never identifies a process across time. The
start time does:

| OS    | Source                                             | `StartTime` value                 |
|-------|----------------------------------------------------|-----------------------------------|
| macOS | libproc `proc_pidinfo(PROC_PIDTBSDINFO)` `pbi_start_tvsec/usec` | ms since the Unix epoch |
| Linux | `/proc/<pid>/stat` field 22 `starttime`            | clock ticks since boot            |

`StartTime` is opaque; only equality matters. `os::is_alive(pid, start_time)` is
true only when `kill(pid, 0)` succeeds (or `EPERM`), the process is not a zombie,
and its start time equals the recorded one. Before signalling a group whose
leader is gone, `stop` checks whether the leader's pid now belongs to a process
with a different start time; if so the old group cannot exist any more (the
kernel does not reuse a pid while a process group with that id exists) and the
handle is reported `AlreadyDead` without sending anything.

## Adoption after a daemon restart

The state file (deliverable 11, [recovery.md](recovery.md)) stores
`AdoptRecord { pid, pgid, start_time, container_id }`. `Runtime::adopt(record)` returns `Handle::Adopted` only if the
pid is alive, its start time matches, and `getpgid(pid) == pgid`. An adopted
process is not our child, so:

- it has **no output stream** (only new output is captured if the stem logs to
  a file; documented limitation of FR-CR-2);
- its exit status is unknown — `wait` polls liveness and returns
  `ExitStatus { code: None, signal: None }`;
- `stop` uses the same group-signal sequence, polling liveness instead of
  `waitpid`.

## Output and backpressure

Two reader tasks (stdout, stderr) split the byte stream into
`OutputLine { ts, stream: Out|Err, text }`:

- split on `\n`, one trailing `\r` stripped (`\r\n` works);
- invalid UTF-8 decoded lossily (U+FFFD);
- a partial last line is emitted at EOF;
- a single line is capped at 64 KiB; the rest is discarded and
  ` [truncated N bytes]` appended.

Lines go into a `tokio::sync::broadcast` channel of **4096** lines per handle.
The sender never blocks, so the pipes are always drained and a log storm cannot
stall the process or grow daemon memory without bound: a subscriber that falls
behind skips the oldest lines. `OutputStream` (the subscription type) turns the
channel's `Lagged(n)` into `OutputEvent::Dropped(n)` and adds `n` to the handle's
counter, visible as `RuntimeFacts::dropped_lines` / `ProcessRuntime::dropped_lines`.
The first subscription after `start` is created before the readers start, so it
sees output from the very first line; later subscriptions see new lines only.
The stream ends (`recv() -> None`) when both pipes are closed — i.e. when the
leader *and* every process that inherited its stdout/stderr have exited.

## Process facts

`describe` returns `RuntimeFacts { pid, pgid, start_time, children, ports,
dropped_lines, container_id: None }`. `children` is `os::process_tree(pgid)`:
every live process in the group with `ppid`, `cpu_time_ms` (user+system) and
`rss_bytes` — macOS via `proc_listpids(PROC_PGRP_ONLY)` + `PROC_PIDTBSDINFO` +
`PROC_PIDTASKINFO` (Mach time converted with `mach_timebase_info`), Linux via a
`/proc/*/stat` scan. `ports` are TCP LISTEN sockets of those pids (macOS `lsof`,
Linux `/proc/net/tcp{,6}` inode → `/proc/<pid>/fd`). `os::listeners_on_port(port)`
answers the reverse question for orphan detection.
