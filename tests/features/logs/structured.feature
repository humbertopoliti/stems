@FR-LG-4
Feature: Structured log detection
  JSON lines are parsed: `level` and `msg` are lifted out and the remaining
  keys are kept as `fields`. Plain lines get a best-effort level from a
  prefix such as `ERROR `.

  Scenario: JSON lines expose level, message and fields
    Given the "minimal" workspace with a local override setting stems.echo-svc.env.SHOP_LOG_JSON="1"
    When I run "stems up --detach --json"
    Then the command succeeds
    When the chaos endpoint "logs?n=1&level=error" is called on "echo-svc"
    Then the chaos response status is 200
    When I run "stems logs echo-svc --json --grep '^GET /__chaos/logs'"
    Then the command succeeds
    And the JSON at "$.level" equals "info"
    And the JSON at "$.text" equals "GET /__chaos/logs 200"
    And the JSON at "$.fields.request_id" exists
    And the JSON at "$.fields.ts" exists
    When I run "stems logs echo-svc --json --level error"
    Then the JSON at "$.text" equals "chaos log line 0"
    And the JSON at "$.fields.level" does not exist
    When I run "stems down --json"
    Then the command succeeds

  Scenario: plain lines get their level from the prefix
    Given the "minimal" workspace is up
    When the chaos endpoint "logs?n=1&level=error" is called on "echo-svc"
    And I run "stems logs echo-svc --json --grep 'chaos log line'"
    Then the command succeeds
    And the JSON at "$.text" equals "ERROR chaos log line 0"
    And the JSON at "$.level" equals "error"
    And the JSON at "$.fields" equals null
    When I run "stems logs echo-svc --json --grep '^INFO listening'"
    Then the JSON at "$.level" equals "info"
    When I run "stems down --json"
    Then the command succeeds
