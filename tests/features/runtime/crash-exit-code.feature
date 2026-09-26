@FR-LC-5
Feature: A crashing process reports its exit code
  The runtime reaps the leader and the daemon emits `process.exited` with
  its exit code.

  Background:
    Given the "minimal" workspace
    And the daemon is started with debug RPCs

  Scenario: /__chaos/crash?code=3 yields process.exited with code 3
    When I call the daemon RPC "_debug.start_raw" with {"spec": {"command": "python3", "args": ["app.py"], "cwd": "${ws}/../../repos/shop-api", "env": {"PORT": "${port:18090}", "SHOP_CHAOS": "1"}}}
    Then the command succeeds
    And I save the JSON at "$.result.pid" as "leader"
    And within 10s port ${port:18090} is listening
    When the chaos endpoint "crash?code=3" is called on port ${port:18090}
    Then within 5s the events stream contains {"kind": "process.exited", "reason": "exited with code 3", "data": {"pid": ${var:leader}, "code": 3, "signal": null}}
    And within 2s none of the pids ${var:leader} is alive
