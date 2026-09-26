@FR-ST-6 @error
Feature: a failing output command fails its stem
  An output command that exits non-zero fails the stem (`SCRIPT_FAILED`,
  `details.output` names the output) and its process is stopped; it never
  becomes `healthy`. Fixture: tests/fixtures/workspaces/outputs-demo
  (`broken-output`'s command exits 2).

  Scenario: the output command exits 2
    Given the fixture workspace "outputs-demo"
    When I run "stems up --detach --json broken-output"
    Then the exit code is 1
    And the JSON error has code "SCRIPT_FAILED"
    And the JSON at "$.errors[0].details.output" equals "BAD"
    And the JSON at "$.errors[0].details.script" equals "outputs"
    And the JSON at "$.errors[0].details.exit" equals 2
    And the JSON at "$.errors[0].details.tail" contains "cannot read the token"
    When I run "stems status --json broken-output"
    Then the JSON at "$.data.stems[0].state" equals "failed"
    And the JSON at "$.data.stems[0].pid" equals null
    And the events stream does not contain {"kind": "stem.state", "stem": "broken-output", "to": "healthy"}
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
