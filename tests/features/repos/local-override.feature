@FR-WS-3
Feature: a local codebase override replaces the git form
  `stems.local.yaml: stems.<s>.codebase: <path>` replaces `{ git: … }`
  entirely: nothing is cloned and `repos status` reports `source: local`.

  Scenario: overriding the codebase with a path skips cloning
    Given the fixture workspace "git-codebase"
    And a local override setting stems.shop-api.codebase=${tmp}/examples/repos/shop-api
    When I run "stems show --json"
    Then the command succeeds
    When I run "stems repos status --json"
    Then the command succeeds
    And the JSON at "$.data.repos[0].stem" equals "shop-api"
    And the JSON at "$.data.repos[0].source" equals "local"
    And the JSON at "$.data.repos[0].url" equals null
    And the JSON at "$.data.repos[0].exists" equals true
    And the JSON at "$.data.repos[0].path" equals "${tmp}/examples/repos/shop-api"
    When I run "stems repos sync --json"
    Then the command succeeds
    And the JSON at "$.data.repos[0].action" equals "skipped_local"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" contains "shop-api"
    And the file ".stems/repos/shop-api" does not exist
    And the events stream does not contain {"kind": "repo.cloned"}
    When I run "stems down --all --json"
    Then the command succeeds
