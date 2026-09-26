@FR-AI-1
Feature: a changed script catalog changes the MCP tools
  Adding a custom script is `scripts_changed` (hot) and changes the script
  catalog (`plan.catalog_changed`). Once applied (here automatically, with
  `config.reload.auto_apply: true`) the daemon emits `tools.changed {added}`,
  which `stems mcp` turns into `notifications/tools/list_changed`; the next
  `tools/list` has the new tool.

  Scenario: a new custom script becomes a tool
    Given the "minimal" workspace
    And a local override setting config.reload.auto_apply=true
    When I run "stems up --detach --json"
    Then the command succeeds
    Given an MCP client is connected
    Then the tools list does not contain "echo_svc__greet"
    Given a local override setting stems.echo-svc.scripts.greet={command: "echo hi", description: "Says hi"}
    Then within 5s the events stream contains {"kind": "config.changed", "data": {"plan": {"catalog_changed": true, "stems": [{"name": "echo-svc", "action": "scripts_changed", "hot": true}]}}}
    And within 5s the events stream contains {"kind": "tools.changed", "data": {"added": ["echo_svc__greet"]}}
    And the tools list contains "echo_svc__greet"
    When I run "stems down --json"
    Then the command succeeds
