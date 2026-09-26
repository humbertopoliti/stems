@FR-WS-6 @error
Feature: Validation reports every error in one run
  Validation never stops at the first problem: all errors are collected and
  returned sorted by file, line and column.
  Fixture: tests/fixtures/workspaces/two-errors (an unknown dependency and a
  static port conflict). Original ports: with the harness's port-remapping
  stems.local.yaml the PORT_CONFLICT would be located in that file, which
  sorts before stems.yaml, and this scenario asserts the order within one file.

  Scenario: two independent errors are both reported
    Given the fixture workspace "two-errors" with its original ports
    When I run "stems validate --json"
    Then the exit code is 2
    And the JSON at "$.ok" equals false
    And the JSON error has code "UNKNOWN_DEPENDENCY" and path "stems.api.depends_on"
    And the JSON error has code "PORT_CONFLICT" and path "stems.web.ports"
    And the JSON at "$.errors[0].code" equals "UNKNOWN_DEPENDENCY"
    And the JSON at "$.errors[1].code" equals "PORT_CONFLICT"
    And the JSON at "$.errors[2]" does not exist
