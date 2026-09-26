@FR-WS-9 @error
Feature: include / extends errors
  Two files of one include level (two included files, or an included file
  and the file including it) defining the same stem is DUPLICATE_STEM, with
  both file paths in `details.files`. Overriding a stem from an `extends`
  base or from stems.local.yaml stays allowed (that is what they are for).
  Fixture: tests/fixtures/workspaces/include-duplicate.

  Scenario: a stem defined in two included files
    Given the fixture workspace "include-duplicate"
    When I run "stems validate --json"
    Then the exit code is 2
    And the JSON error has code "DUPLICATE_STEM" and path "stems.pay"
    And the JSON at "$.errors[0].details.stem" equals "pay"
    And the JSON at "$.errors[0].details.files[0]" contains "teams/a.yaml"
    And the JSON at "$.errors[0].details.files[1]" contains "teams/b.yaml"
    And the error message contains "defined in both"

  Scenario: stems.local.yaml may still override an included stem
    Given the fixture workspace "include-demo"
    And a local override setting stems.payments.enabled=false
    When I run "stems show --json"
    Then the command succeeds
    And the JSON at "$.data.stems.payments.enabled" equals false
