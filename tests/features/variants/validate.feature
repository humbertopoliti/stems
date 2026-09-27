@FR-ST-8 @FR-WS-6
Feature: variants are resolved and validated like the rest of the config
  A variant is a partial stem definition merged over the base stem when it
  is active (`stems.<stem>.variant`); `stems show` reports the active
  `variant`, the `variants` declared and the resolved form. Variant fields
  go through the same schema: a bad field is SCHEMA_INVALID at its path
  under `variants`; selecting a missing variant is UNKNOWN_VARIANT.
  Fixture: tests/fixtures/workspaces/variants-demo.

  Scenario: show reports the variants and the resolved type of each choice
    Given the fixture workspace "variants-demo"
    When I run "stems show api --json"
    Then the command succeeds
    And the JSON at "$.data.variant" equals "local"
    And the JSON at "$.data.variants" equals ["slow", "docker"]
    And the JSON at "$.data.type" equals "process"
    And the JSON at "$.data.command" equals "python3 app.py"
    When I run "stems switch api docker --no-apply --json"
    Then the command succeeds
    When I run "stems validate --skip-requires --json"
    Then the command succeeds
    When I run "stems show api --json"
    Then the JSON at "$.data.variant" equals "docker"
    And the JSON at "$.data.type" equals "docker"
    And the JSON at "$.data.command" equals null
    And the JSON at "$.data.build.context" contains "examples/repos/shop-api"
    And the JSON at "$.data.env.SHOP_CHAOS" equals "1"
    And the JSON at "$.data.health.start_timeout" equals "3m"
    And the JSON at "$.data.depends_on" equals []
    When I run "stems switch api slow --no-apply --json"
    And I run "stems show api --json"
    Then the JSON at "$.data.type" equals "process"
    And the JSON at "$.data.env.SHOP_SLEEP_START" equals "1"
    When I run "stems show web --json"
    Then the JSON at "$.data.variant" does not exist
    And the JSON at "$.data.variants" does not exist

  @error
  Scenario: an invalid variant field is SCHEMA_INVALID at its path
    Given the fixture workspace "variants-demo" with its original ports
    And the local override file contains:
      """
      stems:
        api:
          variants:
            docker:
              imag: shop:1
      """
    When I run "stems validate --skip-requires --json"
    Then the exit code is 2
    And the JSON error has code "SCHEMA_INVALID" and path "stems.api.variants.docker.imag"

  @error
  Scenario: selecting a variant that does not exist is UNKNOWN_VARIANT
    Given the fixture workspace "variants-demo" with its original ports
    And the local override file contains:
      """
      stems:
        api:
          variant: dockr
      """
    When I run "stems validate --skip-requires --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_VARIANT" and path "stems.api.variant"
