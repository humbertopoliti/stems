@FR-LC-6
Feature: a user stop is never undone by the restart policy
  The actor knows the intent: an exit caused by `stems stop`/`down`/
  `restart` is not a crash, even with `restart.policy: always`. A stop
  while a restart is pending cancels its backoff timer.

  Scenario: policy always, stems stop stays stopped
    Given the fixture workspace "restart"
    And a local override setting stems.api.restart.policy=always
    When I run "stems up api --detach --json"
    Then the command succeeds
    When I run "stems stop api --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals ["api"]
    And within 5s the stem "api" is "stopped"
    And during 3s the events stream never contains {"kind": "stem.restarting", "stem": "api"}
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].state" equals "stopped"
    And the JSON at "$.data.stems[0].restarts" equals 0
    When I run "stems down --json"
    Then the command succeeds

  Scenario: a stop during the backoff cancels the pending restart
    Given the fixture workspace "restart"
    And a local override setting stems.api.restart.backoff.initial=4s
    When I run "stems up api --detach --json"
    Then the command succeeds
    When the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s the events stream contains {"kind": "stem.restarting", "stem": "api", "data": {"attempt": 1, "delay_ms": 4000}}
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].state" equals "starting"
    And the JSON at "$.data.stems[0].reason" equals "restarting in 4s (attempt 1)"
    And the JSON at "$.data.stems[0].pid" equals null
    When I run "stems stop api --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals ["api"]
    And the last command took less than 5000 ms
    And within 5s the stem "api" is "stopped"
    # Past the moment the cancelled timer would have fired (4 s after the
    # crash): no respawn, so the stem never became healthy a second time.
    And during 6s the events stream never contains {"kind": "stem.restarting", "stem": "api", "data": {"attempt": 2}}
    And there are exactly 1 events matching {"kind": "stem.state", "stem": "api", "to": "healthy"}
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].state" equals "stopped"
    And the JSON at "$.data.stems[0].pid" equals null
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
