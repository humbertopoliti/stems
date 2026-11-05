@FR-ST-5
Feature: named-port variables
  Besides `PORT` and `STEMS_<DEP>_PORT` (the primary port), every explicitly
  named port is exported: `PORT_<NAME>` for the stem's own ports and
  `STEMS_<DEP>_PORT_<NAME>` for each dependency's.

  Scenario: a dependant and the stem itself see named ports
    Given the fixture workspace "auto-port"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status -v --json"
    Then the command succeeds
    When I save the JSON at "$.data.stems[0].ports[0].port" as "api_port"
    Then the JSON at "$.data.stems[0].env.PORT_HTTP" equals "${var:api_port}"
    And the JSON at "$.data.stems[1].env.STEMS_API_PORT_HTTP" equals "${var:api_port}"
    And the JSON at "$.data.stems[1].env.STEMS_API_PORT" equals "${var:api_port}"
    And the JSON at "$.data.stems[1].env.PORT_HTTP" exists
    When I run "stems down --all --json"
    Then the command succeeds
