@FR-LC-6 @error
Feature: a stem that keeps crashing is given up on
  Beyond `restart.max` restarts within `restart.window` the next crash is
  not restarted: the stem is `failed` with `MAX_RESTARTS` (details:
  attempts, max, window_ms, exit_code) and the daemon emits
  `stem.gave_up`. Nothing of the stem is left running.

  Scenario: max 2, the third crash gives up
    Given the fixture workspace "restart"
    And a local override setting stems.api.restart.max=2
    When I run "stems up api --detach --json"
    Then the command succeeds
    When I run "stems status api --json"
    And I save the JSON at "$.data.stems[0].pid" as "pid1"
    And the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s the events stream contains {"kind": "stem.restarting", "stem": "api", "data": {"attempt": 1}}
    And within 15s the stem "api" is "healthy"
    When I run "stems status api --json"
    And I save the JSON at "$.data.stems[0].pid" as "pid2"
    And the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s the events stream contains {"kind": "stem.restarting", "stem": "api", "data": {"attempt": 2}}
    And within 15s the stem "api" is "healthy"
    When I run "stems status api --json"
    And I save the JSON at "$.data.stems[0].pid" as "pid3"
    And the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s the events stream contains {"kind": "stem.gave_up", "stem": "api", "data": {"attempts": 2, "max": 2, "window_ms": 600000, "exit_code": 3}}
    And within 5s the stem "api" is "failed"
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].error.code" equals "MAX_RESTARTS"
    And the JSON at "$.data.stems[0].error.details.attempts" equals 2
    And the JSON at "$.data.stems[0].glyph" equals "failed"
    And the JSON at "$.data.stems[0].pid" equals null
    And the JSON at "$.data.stems[0].restarts" equals 2
    And the JSON at "$.data.summary.failed" equals 1
    And there are exactly 2 events matching {"kind": "stem.restarting", "stem": "api"}
    And within 5s none of the pids ${var:pid1} is alive
    And within 5s none of the pids ${var:pid2} is alive
    And within 5s none of the pids ${var:pid3} is alive
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
