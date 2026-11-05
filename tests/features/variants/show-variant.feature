@FR-ST-8
Feature: stems show --variant previews a variant; inherited_from says where each field comes from
  `stems show <stem> --variant <name>` resolves the stem with that variant
  active, without touching stems.local.yaml (that is `stems switch`).
  `inherited_from` maps every top-level field (and each `env`, `scripts`
  and `outputs` key) set by some layer to the layer that set it last:
  `base`, `configuration`, `variant`, `local`, or `removed` for a `null`.
  Fixture: tests/fixtures/workspaces/variants-demo (the harness's port
  remapping is the `local` layer).

  Scenario: show a variant without switching to it
    Given the fixture workspace "variants-demo"
    When I run "stems show api --variant docker --json"
    Then the command succeeds
    And the JSON at "$.data.variant" equals "docker"
    And the JSON at "$.data.type" equals "docker"
    And the JSON at "$.data.inherited_from.type" equals "variant"
    And the JSON at "$.data.inherited_from.build" equals "variant"
    And the JSON at "$.data.inherited_from['env.SHOP_CHAOS']" equals "base"
    And the JSON at "$.data.inherited_from['scripts.setup']" equals "base"
    And the JSON at "$.data.inherited_from.ports" equals "local"
    And the JSON at "$.data.inherited_from.command" does not exist
    When I run "stems show api --json"
    Then the command succeeds
    And the JSON at "$.data.variant" equals "local"
    And the JSON at "$.data.type" equals "process"
    And the JSON at "$.data.inherited_from.type" equals "base"
    And the JSON at "$.data.inherited_from.command" equals "base"
    When I run "stems show api --variant local --json"
    Then the command succeeds
    And the JSON at "$.data.variant" equals "local"

  Scenario: show --variant can go back to the base of a switched stem
    Given the fixture workspace "variants-demo"
    When I run "stems switch api slow --no-apply --json"
    Then the command succeeds
    When I run "stems show api --variant base --json"
    Then the command succeeds
    And the JSON at "$.data.variant" equals "local"
    And the JSON at "$.data.env.SHOP_SLEEP_START" does not exist
    When I run "stems show api --json"
    Then the JSON at "$.data.variant" equals "slow"
    And the JSON at "$.data.inherited_from['env.SHOP_SLEEP_START']" equals "variant"

  @error
  Scenario: an unknown variant is UNKNOWN_VARIANT
    Given the fixture workspace "variants-demo"
    When I run "stems show api --variant nope --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_VARIANT"
    And the JSON at "$.errors[0].details.known" equals ["slow", "docker"]

  @error
  Scenario: --variant needs a stem
    Given the fixture workspace "variants-demo"
    When I run "stems show --variant docker --json"
    Then the exit code is 2
    And the JSON error has code "USAGE"
