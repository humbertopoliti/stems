@FR-ST-6 @error
Feature: an output reference without a value fails the dependant's start
  Output references are resolved when the dependant starts. One that has no
  value then (the dependency does not declare it, or is not a `condition:
  healthy` dependency that has become healthy) is `UNRESOLVED_VARIABLE` and
  the dependant is never spawned; `up` exits 3 (api did come up). Fixture:
  tests/fixtures/workspaces/outputs-demo (`needs-missing` references
  `${stem.api.outputs.NOPE}`).

  Scenario: a dependant references an output its dependency does not publish
    Given the fixture workspace "outputs-demo"
    When I run "stems up --detach --json needs-missing"
    Then the exit code is 3
    And the JSON at "$.data.ready" equals ["api"]
    And the JSON at "$.data.failed[0].stem" equals "needs-missing"
    And the JSON error has code "UNRESOLVED_VARIABLE"
    And the JSON at "$.errors[0].details.reference" equals "stem.api.outputs.NOPE"
    And the JSON at "$.errors[0].details.key" equals "X"
    And the JSON at "$.errors[0].hint" contains "outputs are available only from dependencies with condition: healthy"
    When I run "stems status --json needs-missing"
    Then the JSON at "$.data.stems[0].state" equals "failed"
    And the JSON at "$.data.stems[0].pid" equals null
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
