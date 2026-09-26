@FR-LC-5 @FR-ST-3 @NFR-2
Feature: Processes run in their own process group
  The runtime starts a process with setsid, so it leads a new process group;
  stopping sends SIGTERM to the whole group, so children forked by the stem
  die with it. Driven through the daemon's `_debug.*` RPCs.

  Background:
    Given the "minimal" workspace
    And the daemon is started with debug RPCs

  Scenario: stopping the group stops every forked child gracefully
    When I call the daemon RPC "_debug.start_raw" with {"spec": {"command": "python3", "args": ["app.py"], "cwd": "${ws}/../../repos/shop-api", "env": {"PORT": "${port:18090}", "SHOP_CHAOS": "1"}}}
    Then the command succeeds
    And the JSON at "$.result.kind" equals "process"
    And I save the JSON at "$.result.id" as "h"
    And I save the JSON at "$.result.pid" as "leader"
    And within 10s port ${port:18090} is listening
    When the chaos endpoint "fork?n=3" is called on port ${port:18090}
    Then the chaos response status is 200
    When I call the daemon RPC "_debug.describe" with {"handle": ${var:h}}
    Then the command succeeds
    And the JSON at "$.result.pgid" equals ${var:leader}
    And the JSON at "$.result.children[3]" exists
    And the JSON at "$.result.children[4]" does not exist
    And the JSON at "$.result.ports" contains ${port:18090}
    And I save the JSON at "$.result.children[*].pid" as "pids"
    When I call the daemon RPC "_debug.stop_raw" with {"handle": ${var:h}, "grace_ms": 1000}
    Then the command succeeds
    And the JSON at "$.result" equals "graceful"
    And within 2s none of the pids ${var:pids} is alive
    And within 5s the events stream contains {"kind": "process.exited", "data": {"pid": ${var:leader}}}
