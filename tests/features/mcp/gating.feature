@FR-AI-3 @error
Feature: destructive MCP calls are gated
  `down` with `all`/`volumes`, `up` with `fresh`, `reset` and `doctor` with
  `fix` need `confirm: true` (DESTRUCTIVE_NOT_CONFIRMED) and the workspace
  setting `agent.allow_destructive: true` (DESTRUCTIVE_NOT_ALLOWED; the hint
  names the setting). `agent.allowed_tools` / `denied_tools` hide tools and
  refuse calls to them.

  Scenario: down --all needs confirm and agent.allow_destructive
    Given the "minimal" workspace is up
    And an MCP client is connected
    When the MCP client calls tool "down" with {"all": true}
    Then the tool result is error
    And the tool result JSON at "$.code" equals "DESTRUCTIVE_NOT_CONFIRMED"
    And the tool result JSON at "$.hint" contains "agent.allow_destructive"
    When the MCP client calls tool "down" with {"all": true, "confirm": true}
    Then the tool result is error
    And the tool result JSON at "$.code" equals "DESTRUCTIVE_NOT_ALLOWED"
    And the tool result JSON at "$.hint" contains "stems config set agent.allow_destructive true"
    When the MCP client calls tool "reset" with {"stems": ["echo-svc"], "confirm": true}
    Then the tool result is error
    And the tool result JSON at "$.code" equals "DESTRUCTIVE_NOT_ALLOWED"
    When the MCP client calls tool "get_status" with {}
    Then the tool result JSON at "$.stems[0].state" equals "healthy"
    Given a local override setting agent.allow_destructive=true
    When the MCP client calls tool "down" with {"all": true, "confirm": true}
    Then the tool result is success
    And the tool result JSON at "$.stopped" equals ["echo-svc"]
    And the tool result JSON at "$.daemon_stopping" equals true

  Scenario: denied tools are hidden and refused
    Given the "minimal" workspace with a local override setting agent.denied_tools=[reset, "*__ping"]
    And an MCP client is connected
    Then the tools list does not contain "reset"
    And the tools list does not contain "echo_svc__ping"
    And the tools list contains "get_status"
    When the MCP client calls tool "reset" with {"stems": ["echo-svc"], "confirm": true}
    Then the tool result is error
    And the tool result JSON at "$.code" equals "USAGE"
    And the tool result JSON at "$.message" contains "disabled"
    When the MCP client calls tool "up" with {"fresh": true}
    Then the tool result is error
    And the tool result JSON at "$.code" equals "DESTRUCTIVE_NOT_CONFIRMED"
    And the lock file and socket do not exist
