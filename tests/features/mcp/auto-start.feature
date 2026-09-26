@FR-AI-1
Feature: stems mcp --auto-start
  With `--auto-start` the MCP server starts the workspace daemon when a
  tool needs it. When the client disconnects, a daemon the server started
  is shut down again if no stem is running; running stems keep it alive.

  Scenario: a tool call starts the daemon and disconnecting stops it
    Given the "minimal" workspace
    And an MCP client is connected with auto-start
    Then the tools list contains "echo_svc__ping"
    And the lock file and socket do not exist
    When the MCP client calls tool "get_status" with {}
    Then the tool result is success
    And the tool result JSON at "$.stems[0].state" equals "stopped"
    When I run "stems daemon status --json"
    Then the command succeeds
    When the MCP client disconnects
    Then the lock file and socket do not exist

  Scenario: running stems keep an auto-started daemon alive
    Given the "minimal" workspace
    And an MCP client is connected with auto-start
    When the MCP client calls tool "up" with {}
    Then the tool result is success
    When the MCP client disconnects
    Then within 5s the stem "echo-svc" is "healthy"
    When I run "stems down --all --json"
    Then the command succeeds
