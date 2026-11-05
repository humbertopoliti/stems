@FR-CL-3 @error
Feature: a requirement sets `version` or `from`, not both
  Fixture requires-both: workspace `node` sets both, stem `api`'s `python3`
  sets neither.

  Scenario: both or neither is SCHEMA_INVALID
    Given the fixture workspace "requires-both" with its original ports
    When I run "stems validate --skip-requires --json"
    Then the exit code is 2
    And the JSON error has code "SCHEMA_INVALID" and path "requires.node"
    And the JSON error has code "SCHEMA_INVALID" and path "stems.api.requires.python3"
