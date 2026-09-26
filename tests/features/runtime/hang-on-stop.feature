@FR-LC-5
Feature: A process that ignores SIGTERM is killed after the grace period
  Stop = SIGTERM to the group, wait the grace period, then SIGKILL.

  Background:
    Given the "minimal" workspace
    And the daemon is started with debug RPCs

  Scenario: stop escalates to SIGKILL after the grace period
    When I call the daemon RPC "_debug.start_raw" with {"spec": {"command": "python3", "args": ["app.py"], "cwd": "${ws}/../../repos/shop-api", "env": {"PORT": "${port:18090}", "SHOP_CHAOS": "1"}}}
    Then the command succeeds
    And I save the JSON at "$.result.id" as "h"
    And within 10s port ${port:18090} is listening
    When the chaos endpoint "fork?n=2" is called on port ${port:18090}
    Then the chaos response status is 200
    When the chaos endpoint "hang-on-stop" is called on port ${port:18090}
    Then the chaos response status is 200
    When I call the daemon RPC "_debug.describe" with {"handle": ${var:h}}
    Then the JSON at "$.result.children[2]" exists
    And I save the JSON at "$.result.children[*].pid" as "pids"
    When I call the daemon RPC "_debug.stop_raw" with {"handle": ${var:h}, "grace_ms": 500}
    Then the command succeeds
    And the JSON at "$.result" equals "killed"
    And the last command took at least 500 ms
    And the last command took less than 4000 ms
    And within 2s none of the pids ${var:pids} is alive
    And within 5s the events stream contains {"kind": "process.exited", "data": {"signal": 9}}
