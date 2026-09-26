@FR-WD-1
Feature: changes inside the debounce window fire once
  Ten files written at once (a shell loop, well under 100 ms) fall into one
  1 s debounce window: exactly one `watch.triggered` listing them
  (`path_count: 10`) and exactly one restart.

  Scenario: ten files in one burst restart the stem once
    Given the fixture workspace "watch-shop"
    And the workspace has a private copy of the repos
    When I run "stems up burst --detach --json"
    Then the command succeeds
    And within 15s the stem "burst" is "healthy"
    When I run the shell command "cd ${tmp}/examples/repos/shop-api && for i in 0 1 2 3 4 5 6 7 8 9; do echo '# burst' > burst_$i.py; done"
    Then the exit code is 0
    And within 5s the events stream contains {"kind": "watch.triggered", "stem": "burst", "data": {"path_count": 10, "action": "restart"}}
    And within 20s the events stream contains {"kind": "watch.action_finished", "stem": "burst", "data": {"ok": true}}
    And during 3s there are at most 1 events matching {"kind": "watch.triggered", "stem": "burst"}
    And there are exactly 1 events matching {"kind": "watch.triggered", "stem": "burst"}
    And there are exactly 1 events matching {"kind": "stem.restarting", "stem": "burst"}
    And within 5s the stem "burst" is "healthy"
    When I run "stems down --json"
    Then the command succeeds
