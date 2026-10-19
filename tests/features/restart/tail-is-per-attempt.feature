@FR-LC-6 @error
Feature: the output tail belongs to the attempt that died
  The `stderr_tail` of a `MAX_RESTARTS` error holds the last lines of the
  *last* attempt only: lines an earlier attempt wrote before its own crash
  are never shown, even though they are still in the stem's log.

  Scenario: max 1: the first crash's line is not in the final tail
    Given the fixture workspace "restart"
    And a local override setting stems.api.restart.max=1
    When I run "stems up api --detach --json"
    Then the command succeeds
    When the chaos endpoint "crash?code=3&msg=first-crash" is called on "api"
    Then within 10s the events stream contains {"kind": "stem.restarting", "stem": "api", "data": {"attempt": 1}}
    And within 15s the stem "api" is "healthy"
    When the chaos endpoint "crash?code=3&msg=second-crash" is called on "api"
    Then within 10s the events stream contains {"kind": "stem.gave_up", "stem": "api", "data": {"attempts": 1, "max": 1, "stderr_tail": ["chaos crash requested code=3 msg=second-crash"], "output_truncated": false}}
    And within 5s the stem "api" is "failed"
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].error.code" equals "MAX_RESTARTS"
    And the JSON at "$.data.stems[0].error.details.stderr_tail[-1]" equals "chaos crash requested code=3 msg=second-crash"
    And the JSON array at "$.data.stems[0].error.details.stderr_tail" has no element containing "first-crash"
    And the JSON at "$.data.stems[0].error.details.stdout_tail" does not exist
    When I run "stems logs api --json"
    Then the command succeeds
    And the JSON array at "$[*].text" has an element containing "first-crash"
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
