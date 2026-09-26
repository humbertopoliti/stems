@FR-ST-5 @FR-WS-10
Feature: port: auto
  A `port: auto` port is allocated when its stem starts (a free ephemeral
  port, kept for the daemon's lifetime and across restarts), announced with
  `stem.port_allocated`, given to the process as `PORT` and substituted
  into dependants' `${stem.<name>.port}` references. `stems status -v`
  shows the environment stems set for each running stem.

  Scenario: the allocated port reaches the stem and its dependant
    Given the fixture workspace "auto-port"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status -v --json"
    Then the JSON at "$.data.stems[0].name" equals "api"
    And the JSON at "$.data.stems[0].ports[0].auto" equals true
    And the JSON at "$.data.stems[0].ports[0].port" is greater than 1024
    When I save the JSON at "$.data.stems[0].ports[0].port" as "api_port"
    Then the JSON at "$.data.stems[0].env.PORT" equals "${var:api_port}"
    And the JSON at "$.data.stems[1].env.API_URL" equals "http://localhost:${var:api_port}"
    And the JSON at "$.data.stems[1].env.STEMS_API_PORT" equals "${var:api_port}"
    And the JSON at "$.data.stems[1].env.STEMS_STEM" equals "web"
    And the JSON at "$.data.stems[1].env.STEMS_WORKSPACE" equals "${ws}"
    And the JSON at "$.data.stems[1].env.STEMS_RUN_ID" exists
    And the JSON at "$.data.stems[1].env.HOME" does not exist
    And within 1s the events stream contains {"kind": "stem.port_allocated", "stem": "api", "data": {"name": "http", "port": ${var:api_port}}}
    When the chaos endpoint "logs?n=1" is called on "api"
    Then the chaos response status is 200
    When I run "stems restart api --json"
    Then the command succeeds
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].ports[0].port" equals ${var:api_port}
    And the JSON at "$.data.stems[0].state" equals "healthy"
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: status without -v has no env
    Given the fixture workspace "auto-port"
    When I run "stems up api --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api"]
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].env" does not exist
    And the JSON at "$.data.stems[1].state" equals "stopped"
    When I run "stems down --all --json"
    Then the command succeeds
