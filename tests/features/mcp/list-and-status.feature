@FR-AI-1
Feature: MCP server tools and status
  `stems mcp` serves the workspace daemon to MCP clients over stdio. Its
  `tools/list` holds every core tool plus one `<stem>__<script>` tool per
  custom script (FR-AI-2); tool results are JSON text.

  Scenario: every core tool and the custom script tool are listed; get_status is healthy
    Given the "minimal" workspace is up
    And an MCP client is connected
    Then the tools list contains "list_stems"
    And the tools list contains "get_status"
    And the tools list contains "get_graph"
    And the tools list contains "start"
    And the tools list contains "stop"
    And the tools list contains "restart"
    And the tools list contains "up"
    And the tools list contains "down"
    And the tools list contains "run_script"
    And the tools list contains "get_logs"
    And the tools list contains "get_metrics"
    And the tools list contains "get_events"
    And the tools list contains "get_config"
    And the tools list contains "doctor"
    And the tools list contains "reset"
    And the tools list contains "watch_pause"
    And the tools list contains "watch_resume"
    And the tools list contains "get_health"
    And the tools list contains "get_outputs"
    And the tools list contains "echo_svc__ping"
    And the tools list does not contain "echo_svc__start"
    When the MCP client calls tool "get_status" with {}
    Then the tool result is success
    And the tool result JSON at "$.stems[0].name" equals "echo-svc"
    And the tool result JSON at "$.stems[0].state" equals "healthy"
    And the tool result JSON at "$.summary.healthy" equals 1
    When the MCP client calls tool "echo_svc__ping" with {}
    Then the tool result is success
    And the tool result JSON at "$.ok" equals true
    And the tool result JSON at "$.tail" equals ["pong"]
    When the MCP client calls tool "list_stems" with {}
    Then the tool result is success
    And the tool result JSON at "$.daemon_running" equals true
    And the tool result JSON at "$.stems[0].state" equals "healthy"
    And the tool result JSON at "$.stems[0].custom_scripts" equals ["ping"]
    When the MCP client calls tool "get_graph" with {}
    Then the tool result is success
    And the tool result JSON at "$.nodes[0].status" equals "healthy"
    When the MCP client calls tool "get_health" with {"stems": ["echo-svc"]}
    Then the tool result is success
    And the tool result JSON at "$.stems[0].name" equals "echo-svc"
    When the MCP client calls tool "get_metrics" with {}
    Then the tool result is success
    When the MCP client calls tool "get_config" with {"stem": "echo-svc"}
    Then the tool result is success
    And the tool result JSON at "$.name" equals "echo-svc"
    When I run "stems down --json"
    Then the command succeeds

  @error
  Scenario: without a daemon, daemon tools fail with DAEMON_NOT_RUNNING and config tools work
    Given the "minimal" workspace
    And an MCP client is connected
    When the MCP client calls tool "get_status" with {}
    Then the tool result is error
    And the tool result JSON at "$.code" equals "DAEMON_NOT_RUNNING"
    And the tool result JSON at "$.hint" contains "--auto-start"
    When the MCP client calls tool "list_stems" with {}
    Then the tool result is success
    And the tool result JSON at "$.daemon_running" equals false
    When the MCP client calls tool "get_graph" with {}
    Then the tool result is success
    And the tool result JSON at "$.live" equals false
    When the MCP client calls tool "no_such_tool" with {}
    Then the tool result is error
    And the tool result JSON at "$.code" equals "USAGE"
    And the lock file and socket do not exist
