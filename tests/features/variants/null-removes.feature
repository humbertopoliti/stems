@FR-ST-8
Feature: null in a later layer removes an inherited entry
  A `null` (`~`) in a variant, a configuration or stems.local.yaml removes
  the key from the merged stem instead of setting it: `scripts: { setup: ~ }`
  drops an inherited setup script, `env: { X: ~ }` an inherited variable.
  `inherited_from` reports such a key as `removed`.
  Fixture: tests/fixtures/workspaces/variants-demo (`api` has a `setup`).

  Scenario: a variant drops the inherited setup script and an env var
    Given the fixture workspace "variants-demo"
    And the local override file is extended with:
      """
      stems:
        api:
          variants:
            bare:
              scripts: { setup: ~ }
              env: { SHOP_CHAOS: ~ }
      """
    When I run "stems validate --skip-requires --json"
    Then the command succeeds
    When I run "stems show api --variant bare --json"
    Then the command succeeds
    And the JSON at "$.data.scripts.setup" does not exist
    And the JSON at "$.data.env.SHOP_CHAOS" does not exist
    And the JSON at "$.data.command" equals "python3 app.py"
    And the JSON at "$.data.inherited_from['scripts.setup']" equals "removed"
    And the JSON at "$.data.inherited_from['env.SHOP_CHAOS']" equals "removed"
    When I run "stems switch api bare --no-apply --json"
    Then the command succeeds
    When I run "stems up api --detach --json"
    Then the command succeeds
    And within 10s the events stream contains {"kind": "stem.state", "stem": "api", "to": "healthy"}
    And the events stream does not contain {"kind": "script.started", "stem": "api", "data": {"script": "setup"}}
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: without the variant the setup script runs
    Given the fixture workspace "variants-demo"
    When I run "stems up api --detach --json"
    Then the command succeeds
    And within 10s the events stream contains {"kind": "script.finished", "stem": "api", "data": {"script": "setup", "exit": 0}}
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: stems.local.yaml removes an env var of the base
    Given the fixture workspace "variants-demo"
    And the local override file is extended with:
      """
      stems:
        api:
          env: { SHOP_CHAOS: ~ }
      """
    When I run "stems show api --json"
    Then the command succeeds
    And the JSON at "$.data.env.SHOP_CHAOS" does not exist
    And the JSON at "$.data.inherited_from['env.SHOP_CHAOS']" equals "removed"
