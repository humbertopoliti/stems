@FR-GR-4
Feature: stems graph exports Mermaid, DOT and JSON
  `--format mermaid` is a `graph LR` flowchart with one line per edge,
  `--format dot` a Graphviz digraph, and `--json` the graph shape
  `{nodes: [{name, type, status, glyph, reason}], edges: [{from, to,
  condition, soft, protocol, via}]}`.

  Scenario: Mermaid has one line per edge
    Given the "hello-shop" workspace
    When I run "stems graph --format mermaid"
    Then the command succeeds
    And stdout contains "graph LR"
    And stdout contains "shop_api -->|healthy| postgres"
    And stdout contains "shop_api -->|healthy| redis"
    And stdout contains "shop_worker -->|healthy| redis"
    And stdout contains "shop_worker -->|seeded| postgres"
    And stdout contains "shop_web -->|healthy| shop_api"

  Scenario: DOT is a digraph
    Given the "hello-shop" workspace
    When I run "stems graph --format dot"
    Then the command succeeds
    And stdout contains "digraph stems {"
    And stdout contains "rankdir=LR;"

  Scenario: JSON has 6 nodes and 5 edges
    Given the "hello-shop" workspace
    When I run "stems graph --json"
    Then the command succeeds
    And the JSON nodes at "$.data.nodes[*].name" equal ["shop-web", "shop-api", "shop-worker", "postgres", "redis", "httpbin"]
    And the JSON nodes at "$.data.edges[*].from" equal ["shop-api", "shop-api", "shop-worker", "shop-worker", "shop-web"]
    And the JSON at "$.data.nodes[?@.name=='postgres'].type" equals "docker"
    And the JSON at "$.data.nodes[?@.name=='httpbin'].status" equals "stopped"
