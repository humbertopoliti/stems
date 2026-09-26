@FR-AI-2 @FR-SC-2
Feature: custom scripts are MCP tools
  Each custom script is published as a tool named `<stem>__<script>`
  (MCP-safe: other characters become `_`) with its `description` and an
  input schema from its `args`. Calling it runs the script like `stems run`
  with the arguments as a JSON object; invalid arguments are a tool error
  with `SCRIPT_ARGS_INVALID`. REQUIREMENTS §7.4's example.

  Scenario: shop_api__create_test_user runs with validated arguments
    Given the fixture workspace "run-scripts"
    When I run "stems up shop-api --detach --json"
    Then the command succeeds
    Given an MCP client is connected
    Then the tools list contains "shop_api__create_test_user"
    And the tools list contains "workspace__needs_api"
    And the tools list does not contain "shop_api__start"
    When the MCP client calls tool "shop_api__create_test_user" with {"email": "x@y.z"}
    Then the tool result is success
    And the tool result JSON at "$.ok" equals true
    And the tool result JSON at "$.argv" equals ["--email", "x@y.z", "--role", "admin"]
    And the tool result JSON at "$.tail[0]" equals "create-test-user: ok (email=x@y.z, role=admin)"
    And within 5s the events stream contains {"kind": "script.finished", "stem": "shop-api", "actor": "mcp:stems-e2e", "data": {"script": "create-test-user", "ok": true}}
    And within 5s the stem log file "shop-api/current.log" contains "created user x@y.z role admin"
    When the MCP client calls tool "shop_api__create_test_user" with {"email": "x@y.z", "role": "superuser"}
    Then the tool result is error
    And the tool result JSON at "$.code" equals "SCRIPT_ARGS_INVALID"
    And the tool result JSON at "$.details.arg" equals "role"
    When the MCP client calls tool "run_script" with {"stem": "shop-api", "name": "create-test-user", "args": {"role": "user"}}
    Then the tool result is error
    And the tool result JSON at "$.code" equals "SCRIPT_ARGS_INVALID"
    And the tool result JSON at "$.details.arg" equals "email"
    When the MCP client calls tool "run_script" with {"stem": "shop-api", "name": "env-args", "args": {"email": "a@b.c", "count": "3"}}
    Then the tool result is success
    And the tool result JSON at "$.tail[0]" contains "count=3"
    When the MCP client calls tool "run_script" with {"stem": "shop-api", "name": "always-fails", "args": {}}
    Then the tool result is error
    And the tool result JSON at "$.ok" equals false
    And the tool result JSON at "$.error.code" equals "SCRIPT_FAILED"
    When I run "stems down --all --json"
    Then the command succeeds
