@FR-DS-2
Feature: Workspace schema_version compatibility
  A stems.yaml written for a newer format than this binary understands is
  refused with SCHEMA_VERSION_UNSUPPORTED (exit 2), naming the stems version
  it needs, and nothing else is reported (the rest of the file may mean
  something else in that format). CLI/daemon version skew is covered by
  daemon/version-mismatch.feature.

  @error
  Scenario: schema_version 99 needs a newer stems
    Given the fixture workspace "schema-future"
    When I run "stems validate --skip-requires --json"
    Then the exit code is 2
    And the JSON error has code "SCHEMA_VERSION_UNSUPPORTED" and path "schema_version"
    And the error message contains "schema_version 99 requires stems >"
    And the JSON at "$.errors[0].details.required_stems" contains ">"
    And the JSON at "$.errors[0].details.supported" equals [1]
    And the JSON at "$.errors[0].hint" contains "stems upgrade"
