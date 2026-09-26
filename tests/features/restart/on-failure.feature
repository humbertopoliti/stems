@FR-LC-6
Feature: a crashed stem is restarted by its on-failure policy
  With `restart.policy: on-failure` (the default) a stem whose process
  exits with a non-zero code is restarted after a backoff: it shows
  `starting` ("restarting in 500ms (attempt 1)"), the daemon emits
  `stem.restarting {attempt, delay_ms, exit_code}`, and the stem comes back
  healthy with a new pid on the same port. The restart runs `pre_start` and
  `post_start` again, never `setup` or `seed` (their stamps are intact).

  Scenario: crash with code 3, restarted once with a new pid
    Given the fixture workspace "restart"
    When I run "stems up hooked --detach --json"
    Then the command succeeds
    And within 10s the JSON at "$.data.stems[0].seeded" of "stems status hooked --json" equals true
    When I save the JSON at "$.data.stems[0].pid" as "pid"
    And the chaos endpoint "crash?code=3" is called on "hooked"
    Then within 10s the events stream contains {"kind": "stem.restarting", "stem": "hooked", "data": {"attempt": 1, "delay_ms": 500, "exit_code": 3, "reason": "exit", "counted": true}}
    And the events stream contains {"kind": "process.exited", "stem": "hooked", "data": {"code": 3}} before {"kind": "stem.restarting", "stem": "hooked"}
    And the events stream contains {"kind": "stem.state", "stem": "hooked", "from": "healthy", "to": "starting", "reason": "restarting in 500ms (attempt 1)"} before {"kind": "stem.restarting", "stem": "hooked"}
    And within 15s the stem "hooked" is "healthy"
    And within 10s the JSON at "$.data.stems[0].seeded" of "stems status hooked --json" equals true
    And the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].pid" does not equal ${var:pid}
    And the JSON at "$.data.stems[0].restarts" equals 1
    And the JSON at "$.data.stems[0].restarts_in_window" equals 1
    And the JSON at "$.data.stems[0].ports[0].port" equals ${port:18492}
    And there are exactly 2 events matching {"kind": "script.started", "stem": "hooked", "data": {"script": "pre_start"}}
    And there are exactly 2 events matching {"kind": "script.started", "stem": "hooked", "data": {"script": "post_start"}}
    And there are exactly 1 events matching {"kind": "script.started", "stem": "hooked", "data": {"script": "setup"}}
    And there are exactly 1 events matching {"kind": "script.started", "stem": "hooked", "data": {"script": "seed"}}
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
