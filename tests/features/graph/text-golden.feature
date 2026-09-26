@FR-GR-4
Feature: stems graph draws the dependency graph as text
  `stems graph` lays the stems out in columns, dependants on the left and
  arrows to their dependencies on the right, with the status glyph in each
  box and the glyph legend below. Without a daemon it is config only: every
  stem is `·`. On a pipe the glyphs stay Unicode unless `STEMS_ASCII` is set;
  `--no-color` only drops the colours.

  Scenario: hello-shop, config only, matches the golden
    Given the "hello-shop" workspace
    When I run "stems graph --no-color"
    Then the command succeeds
    And the output matches golden "graph-hello-shop"

  Scenario: --focus keeps a stem and its neighbours
    Given the "hello-shop" workspace
    When I run "stems graph --no-color --focus shop-api"
    Then the command succeeds
    And the output matches golden "graph-focus"
    And stdout does not contain "shop-worker"

  @error
  Scenario: --focus on an unknown stem is a usage error
    Given the "hello-shop" workspace
    When I run "stems graph --focus nope --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_STEM"

  Scenario: --profile draws the profile's stems and their hard dependencies
    Given the "hello-shop" workspace
    When I run "stems graph --profile backend --json"
    Then the command succeeds
    And the JSON nodes at "$.data.nodes[*].name" equal ["shop-api", "shop-worker", "postgres", "redis"]

  Scenario: STEMS_ASCII switches to the ASCII fallback
    Given the "hello-shop" workspace
    When I run "stems graph --no-color" with env STEMS_ASCII=1
    Then the command succeeds
    And stdout contains "| shop-web - +----->| shop-api"
    And stdout contains "OK healthy  WARN degraded"
