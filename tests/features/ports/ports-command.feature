@FR-ST-5
Feature: stems ports
  `stems ports` lists every stem's host ports from the config, with the
  live `auto` allocations when a daemon runs, and the variables and
  templates that address them, so files outside stems never repeat a number.
  It never starts the daemon.

  Scenario: without a daemon an auto port is null and not in vars
    Given the fixture workspace "auto-port"
    When I run "stems ports --json"
    Then the command succeeds
    And the JSON at "$.data.ports[?@.stem=='api'].port" equals null
    And the JSON at "$.data.ports[?@.stem=='api'].auto" equals true
    And the JSON at "$.data.ports[?@.stem=='api'].primary" equals true
    And the JSON at "$.data.ports[?@.stem=='api'].var" equals "STEMS_API_PORT"
    And the JSON at "$.data.ports[?@.stem=='api'].template" equals "${stem.api.port}"
    And the JSON at "$.data.ports[?@.stem=='api'].running" equals false
    And the JSON at "$.data.ports[?@.stem=='web'].port" is greater than 1024
    And the JSON at "$.data.ports[?@.stem=='web'].auto" equals false
    And the JSON at "$.data.vars.STEMS_WEB_PORT" is greater than 1024
    And the JSON at "$.data.vars.STEMS_WEB_PORT_HTTP" is greater than 1024
    And the JSON at "$.data.vars.STEMS_API_PORT" does not exist
    When I run "stems status --json"
    Then the command fails

  Scenario: after up the auto port is allocated; filters and formats
    Given the fixture workspace "auto-port"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems ports --json"
    Then the command succeeds
    And the JSON at "$.data.ports[?@.stem=='api'].port" is greater than 1024
    And the JSON at "$.data.ports[?@.stem=='api'].running" equals true
    When I save the JSON at "$.data.ports[?@.stem=='api'].port" as "api_port"
    Then the JSON at "$.data.vars.STEMS_API_PORT" equals ${var:api_port}
    And the JSON at "$.data.vars.STEMS_API_PORT_HTTP" equals ${var:api_port}
    When I run "stems ports api --json"
    Then the JSON at "$.data.ports[0].stem" equals "api"
    And the JSON at "$.data.ports[1]" does not exist
    When I run "stems ports --format dotenv"
    Then the command succeeds
    And stdout contains "STEMS_API_PORT=${var:api_port}"
    And stdout contains "STEMS_WEB_PORT="
    When I run "stems ports --format shell"
    Then the command succeeds
    And stdout contains "export STEMS_API_PORT=${var:api_port}"
    When I run "stems ports --no-status --json"
    Then the JSON at "$.data.ports[?@.stem=='api'].port" equals null
    And the JSON at "$.data.ports[?@.stem=='api'].running" equals false
    When I run "stems down --all --json"
    Then the command succeeds

  Scenario: an unknown stem is an error
    Given the fixture workspace "auto-port"
    When I run "stems ports nope --json"
    Then the command fails
    And the JSON at "$.errors[0].code" equals "UNKNOWN_STEM"
