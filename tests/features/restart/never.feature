@FR-LC-6
Feature: policy never leaves an exited stem alone
  With `restart.policy: never` an exit is final: code 0 is `stopped`,
  anything else `failed` (reason `exited with code N`). No
  `stem.restarting` is emitted.

  Scenario: a clean exit (code 0) is stopped
    Given the fixture workspace "restart"
    And a local override setting stems.api.restart.policy=never
    When I run "stems up api --detach --json"
    Then the command succeeds
    When the chaos endpoint "crash?code=0" is called on "api"
    Then within 10s the stem "api" is "stopped"
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].reason" equals "exited with code 0"
    And the JSON at "$.data.stems[0].pid" equals null
    And the JSON at "$.data.stems[0].restarts" equals 0
    And the events stream does not contain {"kind": "stem.restarting"}
    When I run "stems down --json"
    Then the command succeeds

  Scenario: a crash (code 3) is failed
    Given the fixture workspace "restart"
    And a local override setting stems.api.restart.policy=never
    When I run "stems up api --detach --json"
    Then the command succeeds
    When the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s the stem "api" is "failed"
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].reason" equals "exited with code 3"
    And the JSON at "$.data.summary.failed" equals 1
    And the events stream does not contain {"kind": "stem.restarting"}
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
