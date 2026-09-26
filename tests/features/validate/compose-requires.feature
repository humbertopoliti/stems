@FR-WS-6
Feature: validate warns when compose stems cannot run
  With compose stems in the workspace, `docker compose` v2 is an implicit
  requirement (deliverable 15). Its absence is a `TOOL_VERSION` *warning*,
  not an error, so CI machines without Docker still validate the config.

  Scenario: no docker on PATH is a warning, and --skip-requires skips it
    Given the fixture workspace "compose-redis"
    When I run "stems validate --json" with env PATH=${outside}
    Then the exit code is 0
    And the JSON at "$.data.ok" equals true
    And the JSON at "$.data.warnings[0].code" equals "TOOL_VERSION"
    And the JSON at "$.data.warnings[0].details.tool" equals "docker compose"
    And the JSON at "$.data.warnings[0].details.stems" equals ["cache"]
    When I run "stems validate --skip-requires --json" with env PATH=${outside}
    Then the command succeeds
    And the JSON at "$.data.warnings" equals []
