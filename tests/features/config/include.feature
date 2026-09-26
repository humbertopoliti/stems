@FR-WS-9
Feature: include and extends
  `extends:` merges a base file first, `include:` files are merged in order
  (paths relative to the including file), and the including file wins.
  Fixture: tests/fixtures/workspaces/include-demo. (Needs `stems show`,
  deliverable 07, and a fixture-workspace step in the harness.)

  Scenario: an included file adds a stem
    Given the fixture workspace "include-demo"
    When I run "stems show --json"
    Then the command succeeds
    And the JSON at "$.data.stems.payments.type" equals "process"
    And the JSON at "$.data.stems.gateway.type" equals "process"
    And the JSON at "$.data.stems.payments.env.GATEWAY_URL" contains "localhost:"

  Scenario: an extends base provides vars that the child overrides
    Given the fixture workspace "include-demo"
    When I run "stems show --json"
    Then the command succeeds
    And the JSON at "$.data.vars.region" equals "eu"
    And the JSON at "$.data.vars.owner" equals "child"
    And the JSON at "$.data.stems.gateway.env.OWNER" equals "child"
    And the JSON at "$.data.logs.keep" equals 3
