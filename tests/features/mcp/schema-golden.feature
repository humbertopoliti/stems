@FR-AI-1
Feature: the MCP tool catalogue is a public contract
  Tool names, descriptions and input schemas of the minimal workspace are
  snapshotted in `schema/mcp-tools.json` (regenerate with `UPDATE_SCHEMA=1
  cargo test -p stems-mcp --test golden`).

  Scenario: tools/list with a running daemon equals the golden
    Given the "minimal" workspace is up
    And an MCP client is connected
    Then the tools list matches "schema/mcp-tools.json"
    When I run "stems down --json"
    Then the command succeeds

  Scenario: tools/list without a daemon equals the golden
    Given the "minimal" workspace
    And an MCP client is connected
    Then the tools list matches "schema/mcp-tools.json"
