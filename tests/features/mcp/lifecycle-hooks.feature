@FR-AI-1 @FR-SC-1
Feature: lifecycle hooks are discoverable over MCP
  `list_stems` carries `lifecycle_hooks`, the fixed lifecycle script names
  in lifecycle order (the same list as `stems scripts --json`), so an agent
  knows which script names are hooks without grepping the docs. It needs
  no daemon.

  Scenario: list_stems names the hooks without a daemon
    Given the "minimal" workspace
    And an MCP client is connected with auto-start
    When the MCP client calls tool "list_stems" with {}
    Then the tool result is success
    And the tool result JSON at "$.lifecycle_hooks[0]" equals "bootstrap"
    And the tool result JSON at "$.lifecycle_hooks" contains "pre_start"
    And the tool result JSON at "$.lifecycle_hooks" contains "teardown"
    And the lock file and socket do not exist
    When the MCP client disconnects
    Then the lock file and socket do not exist
