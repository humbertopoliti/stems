@FR-CL-3 @error
Feature: doctor checks the tool versions of `requires:`
  broken/tool-version requires `node >=99`. A fake `node` on PATH makes the
  found version (or its absence) deterministic.

  Scenario: an installed tool that is too old fails requires.node
    Given the broken workspace "tool-version"
    And a fake tool "node" on PATH that runs "echo v20.11.1"
    When I run "stems doctor --json"
    Then the exit code is 1
    And the JSON at "$.data.ok" equals false
    And the JSON at "$.data.checks[?@.id == 'requires.node'].status" equals "fail"
    And the JSON at "$.data.checks[?@.id == 'requires.node'].message" contains "20.11.1"
    And the JSON at "$.data.checks[?@.id == 'requires.node'].message" contains ">=99"
    And the JSON at "$.data.checks[?@.id == 'requires.node'].details.found" equals "20.11.1"
    And the JSON at "$.data.checks[?@.id == 'requires.node'].fixable" equals false
    And the JSON error has code "TOOL_VERSION"
    And the JSON at "$.errors[0].details.check" equals "requires.node"

  Scenario: a missing tool fails requires.node with "not found"
    Given the broken workspace "tool-version"
    And a fake tool "node" on PATH that runs "exit 127"
    When I run "stems doctor --json"
    Then the exit code is 1
    And the JSON at "$.data.checks[?@.id == 'requires.node'].status" equals "fail"
    And the JSON at "$.data.checks[?@.id == 'requires.node'].message" contains "not found"
    And the JSON at "$.data.checks[?@.id == 'requires.node'].message" contains ">=99"
    And the JSON error has code "TOOL_VERSION"
