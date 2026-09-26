@FR-WD-1
Feature: a watch rule restarts its stem when a matching file changes
  `watch: [{paths: ["*.py"], action: restart, debounce: 200ms}]` watches
  the stem's codebase. A change fires `watch.triggered {paths, path_count,
  action, rule_index}`, the stem restarts through the policy bypass (hooks
  run, `stem.restarting` has `counted: false`, the `restarts` counter does
  not move) and `watch.action_finished {ok}` follows. FSEvents on macOS
  delivers changes 0.5–1 s late, so the bounds are generous (5 s).

  Scenario: appending to app.py restarts shop-api
    Given the fixture workspace "watch-shop"
    And the workspace has a private copy of the repos
    When I run "stems up api --detach --json"
    Then the command succeeds
    And within 15s the stem "api" is "healthy"
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].watch" equals {"paused": false, "rules": 1}
    When I save the JSON at "$.data.stems[0].pid" as "pid"
    And I run the shell command "echo '# touched' >> ${tmp}/examples/repos/shop-api/app.py"
    Then within 5s the events stream contains {"kind": "watch.triggered", "stem": "api", "actor": "watchdog", "data": {"paths": ["app.py"], "path_count": 1, "action": "restart", "rule_index": 0}}
    And within 20s the events stream contains {"kind": "watch.action_finished", "stem": "api", "data": {"action": "restart", "ok": true}}
    And within 5s the stem "api" is "healthy"
    And within 1s the events stream contains {"kind": "stem.restarting", "stem": "api", "data": {"counted": false}}
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].pid" does not equal ${var:pid}
    And the JSON at "$.data.stems[0].restarts" equals 0
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
