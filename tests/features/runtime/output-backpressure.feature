@slow @FR-LC-5 @NFR-2
Feature: A log storm cannot exhaust the daemon
  Output goes through a bounded channel per process: a subscriber that
  cannot keep up skips lines (counted as `dropped_lines`) instead of the
  daemon buffering without bound or the process blocking on its pipe.

  Background:
    Given the "minimal" workspace
    And the daemon is started with debug RPCs

  Scenario: 200 000 log lines keep the daemon under 100 MB and count drops
    When I call the daemon RPC "_debug.start_raw" with {"spec": {"command": "python3", "args": ["app.py"], "cwd": "${ws}/../../repos/shop-api", "env": {"PORT": "${port:18090}", "SHOP_CHAOS": "1"}}}
    Then the command succeeds
    And I save the JSON at "$.result.id" as "h"
    And within 10s port ${port:18090} is listening
    When the chaos endpoint "logs?n=200000" is called on port ${port:18090}
    Then the chaos response status is 200
    And the daemon RSS is below 100 MB
    When I call the daemon RPC "_debug.describe" with {"handle": ${var:h}}
    Then the command succeeds
    And the JSON at "$.result.dropped_lines" is greater than 0
    When I call the daemon RPC "_debug.stop_raw" with {"handle": ${var:h}, "grace_ms": 1000}
    Then the JSON at "$.result" equals "graceful"
