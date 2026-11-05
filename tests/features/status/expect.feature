@FR-HS-2 @FR-SC-6
Feature: stems status --expect waits for and checks stem states
  `stems status --expect <healthy|stopped|failed|running> [stems…]
  [--timeout D]` exits 0 when every named (else every enabled) stem is in
  that state, else 1 (`STATUS_MISMATCH`, `data.mismatch: [{stem, state}]`);
  `--timeout` keeps checking until the deadline (deliverable 49).

  Scenario: --timeout waits for a stem that is starting
    Given the fixture workspace "script-preconditions"
    And the daemon is started
    When I run "stems start api --json" in the background
    And I run "stems status --expect healthy api --timeout 15s --json"
    Then the command succeeds
    And the JSON at "$.data.expect" equals "healthy"
    And the JSON at "$.data.mismatch" equals []
    And within 15s the background command exits with code 0
    When I run "stems status --expect running --json"
    Then the exit code is 1
    And the JSON at "$.data.mismatch" equals [{"stem": "web", "state": "stopped"}]
    When I run "stems status --expect stopped api --timeout 300ms --json"
    Then the exit code is 1
    And the JSON error has code "STATUS_MISMATCH"
    And the JSON at "$.data.mismatch[0].state" equals "healthy"
    When I run "stems run web api-path --json"
    Then the command succeeds
    And the JSON at "$.data.tail[0]" contains "API=/"
    And the JSON at "$.data.tail[0]" contains "examples/repos/shop-api"
    When I run "stems down --all --json"
    Then the command succeeds

  @error
  Scenario: without a daemon a named stem must exist
    Given the fixture workspace "script-preconditions"
    When I run "stems status --expect stopped api --json"
    Then the command succeeds
    When I run "stems status --expect stopped nope --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_STEM"
