@FR-WD-1
Feature: changes during a restart queue one more restart, not many
  While a watchdog action runs, further changes pile up in ONE pending
  batch per rule; it fires once the action finished (here after 1 s of
  quiet, `settle: 1s`). Two seconds of changes during and after the first
  restart give at most one extra restart: at most 2 `watch.triggered`, at
  most 2 restarts (3 pids).

  Scenario: two seconds of edits while restarting
    Given the fixture workspace "watch-shop"
    And the workspace has a private copy of the repos
    When I run "stems up steady --detach --json"
    Then the command succeeds
    And within 15s the stem "steady" is "healthy"
    When I run the shell command "echo '# first' >> ${tmp}/examples/repos/shop-api/app.py"
    Then within 6s the events stream contains {"kind": "watch.triggered", "stem": "steady"}
    When I run the shell command "cd ${tmp}/examples/repos/shop-api && i=0; while [ $i -lt 20 ]; do echo '# again' >> app.py; sleep 0.1; i=$((i+1)); done"
    Then the exit code is 0
    And within 25s there are at least 2 events matching {"kind": "watch.action_finished", "stem": "steady", "data": {"ok": true}}
    And during 3s there are at most 2 events matching {"kind": "watch.triggered", "stem": "steady"}
    And there are exactly 2 events matching {"kind": "watch.triggered", "stem": "steady"}
    And there are exactly 2 events matching {"kind": "stem.restarting", "stem": "steady"}
    And within 5s the stem "steady" is "healthy"
    When I run "stems down --json"
    Then the command succeeds
