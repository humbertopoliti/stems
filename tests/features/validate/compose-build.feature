@FR-ST-3
Feature: the `build` field takes one form per stem type
  A compose stem's `build` is a bool (default true: `compose up --build`);
  a docker stem's `build` is an object (`context`, `dockerfile`). The wrong
  form is `SCHEMA_INVALID` at `stems.<n>.build`, and `stems show` reports
  the compose default.

  Scenario: an object on a compose stem is rejected
    Given the fixture workspace "compose-build"
    And a local override setting stems.api.build.context=.
    When I run "stems validate --json"
    Then the exit code is 2
    And the JSON error has code "SCHEMA_INVALID" and path "stems.api.build"
    And the error message contains "compose"

  Scenario: a bool on a docker stem is rejected
    Given the fixture workspace "docker-build"
    And a local override setting stems.api.build=true
    When I run "stems validate --json"
    Then the exit code is 2
    And the JSON error has code "SCHEMA_INVALID" and path "stems.api.build"
    And the error message contains "docker"

  Scenario: show reports build true by default on a compose stem
    Given the fixture workspace "compose-build"
    When I run "stems show api --json"
    Then the command succeeds
    And the JSON at "$.data.build" equals true
    And the JSON at "$.data.type" equals "compose"
