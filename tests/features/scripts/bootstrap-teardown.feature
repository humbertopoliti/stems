@FR-SC-6 @FR-LC-2
Feature: workspace bootstrap and teardown
  The workspace `bootstrap` script runs once per `up`, before any stem; if
  it fails, `up` aborts with SETUP_FAILED and nothing starts. `teardown`
  runs at the end of `down --all`. Workspace scripts run in the integration
  repo.

  Scenario: bootstrap runs before the stems, teardown after down --all
    Given the fixture workspace "scripts-workspace"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the file "workspace.log" contains "bootstrap "
    And the events stream contains {"kind": "script.finished", "data": {"script": "bootstrap", "exit": 0}} before {"kind": "stem.state", "stem": "echo-svc", "to": "starting"}
    When I run "stems logs _workspace --script bootstrap --json"
    Then the command succeeds
    And the JSON at "$..text" contains "bootstrap "
    When I run "stems down --all --json"
    Then the command succeeds
    And the file "workspace.log" contains "teardown"
    And no process from the workspace's process groups is alive

  @error
  Scenario: a failing bootstrap starts nothing
    Given the fixture workspace "scripts-bootstrap-fail"
    When I run "stems up --detach --json"
    Then the exit code is 1
    And the JSON error has code "SETUP_FAILED"
    And the JSON at "$.errors[0].details.script" equals "bootstrap"
    And the JSON at "$.errors[0].details.exit" equals 4
    And the JSON at "$.errors[0].details.tail" contains "bootstrap: docker missing"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].state" equals "stopped"
    And the events stream does not contain {"kind": "stem.state", "to": "starting"}
    When I run "stems down --all --json"
    Then the command succeeds
