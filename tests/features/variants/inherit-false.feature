@FR-ST-8
Feature: a variant with inherit: false does not inherit the base definition
  `inherit: false` merges the variant over an empty stem, so it must be a
  complete definition (`type` included); only `enabled` and the variant
  selection come from the base. `inherit` is only valid inside a variant.
  Fixture: tests/fixtures/workspaces/variants-demo.

  Scenario: an inherit: false variant has none of the base's fields
    Given the fixture workspace "variants-demo"
    And the local override file is extended with:
      """
      stems:
        api:
          variants:
            standalone:
              inherit: false
              type: process
              command: python3 -m http.server
      """
    When I run "stems validate --skip-requires --json"
    Then the command succeeds
    When I run "stems show api --variant standalone --json"
    Then the command succeeds
    And the JSON at "$.data.type" equals "process"
    And the JSON at "$.data.command" equals "python3 -m http.server"
    And the JSON at "$.data.env.SHOP_CHAOS" does not exist
    And the JSON at "$.data.scripts.setup" does not exist
    And the JSON at "$.data.inherited_from.command" equals "variant"
    And the JSON at "$.data.inherited_from.ports" equals "local"
    And the JSON at "$.data.inherited_from['env.SHOP_CHAOS']" does not exist
    And the JSON at "$.data.inherited_from.description" does not exist

  @error
  Scenario: an inherit: false variant without a type is SCHEMA_INVALID
    Given the fixture workspace "variants-demo" with its original ports
    And the local override file contains:
      """
      stems:
        api:
          variant: partial
          variants:
            partial:
              inherit: false
              command: python3 app.py
      """
    When I run "stems validate --skip-requires --json"
    Then the exit code is 2
    And the JSON error has code "SCHEMA_INVALID" and path "stems.api.type"

  @error
  Scenario: inherit on a stem itself is SCHEMA_INVALID
    Given the fixture workspace "variants-demo" with its original ports
    And the local override file contains:
      """
      stems:
        api:
          inherit: false
      """
    When I run "stems validate --skip-requires --json"
    Then the exit code is 2
    And the JSON error has code "SCHEMA_INVALID" and path "stems.api.inherit"
