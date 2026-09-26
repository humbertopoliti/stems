@FR-SC-2 @FR-AI-2
Feature: the script catalogue
  `stems scripts [stem] --json` lists every runnable script — workspace
  scripts first, then each enabled stem's, lifecycle and custom — with its
  description, argument schema, `requires`, and the MCP tool name and input
  schema the MCP server (31) publishes. It loads the config locally: no
  daemon is needed or started.

  Scenario: hello-shop's catalogue
    Given the "hello-shop" workspace
    When I run "stems scripts --json"
    Then the command succeeds
    And the JSON at "$.data.scripts[0].stem" equals null
    And the JSON at "$.data.scripts[?@.name == 'nuke-databases'].description" equals "Drop and recreate all local databases"
    And the JSON at "$.data.scripts[?@.name == 'nuke-databases'].requires" equals ["postgres"]
    And the JSON at "$.data.scripts[?@.name == 'nuke-databases'].mcp_tool" equals "workspace__nuke_databases"
    And the JSON at "$.data.scripts[?@.name == 'bootstrap'].kind" equals "lifecycle"
    And the JSON at "$.data.scripts[?@.name == 'create-test-user'].kind" equals "custom"
    And the JSON at "$.data.scripts[?@.name == 'create-test-user'].description" equals "Create a user with a known password for manual testing"
    And the JSON at "$.data.scripts[?@.name == 'create-test-user'].mcp_tool" equals "shop_api__create_test_user"
    And the JSON at "$.data.scripts[?@.name == 'create-test-user'].args[0].name" equals "email"
    And the JSON at "$.data.scripts[?@.name == 'create-test-user'].input_schema.required" equals ["email"]
    And the JSON at "$.data.scripts[?@.name == 'create-test-user'].input_schema.properties.role.enum" equals ["admin", "user"]
    And the JSON at "$.data.scripts[?@.name == 'seed-large'].input_schema.properties.rows.default" equals 1000000
    And the JSON at "$.data.scripts[?@.stem == 'shop-api' && @.name == 'setup'].kind" equals "lifecycle"
    And the JSON at "$.data.scripts[?@.stem == 'httpbin']" does not exist
    And the lock file and socket do not exist

  Scenario: one stem's scripts, and an unknown stem
    Given the "hello-shop" workspace
    When I run "stems scripts shop-api --json"
    Then the command succeeds
    And the JSON at "$.data.scripts[?@.stem != 'shop-api']" does not exist
    And the JSON at "$.data.scripts[?@.name == 'create-test-user'].stem" equals "shop-api"
    When I run "stems scripts nope --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_STEM"

  Scenario: the daemon serves the same catalogue
    Given the "minimal" workspace
    And the daemon is started
    When I call the daemon RPC "script_catalog" with {"stem": "echo-svc"}
    Then the JSON at "$.result.scripts[?@.name == 'ping'].description" equals "Prints pong"
    And the JSON at "$.result.scripts[?@.name == 'ping'].mcp_tool" equals "echo_svc__ping"
    When I call the daemon RPC "run_script" with {"stem": "echo-svc", "name": "ping", "args": {}}
    Then the JSON at "$.result.ok" equals true
    And the JSON at "$.result.tail" equals ["pong"]
