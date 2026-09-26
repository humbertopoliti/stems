@FR-GR-5
Feature: edge metadata labels the graph
  A `depends_on` edge can carry `protocol` and `via`; `stems graph --edges`
  labels the edge with them (text and Mermaid), and the JSON shape always
  has them.

  Scenario: --edges shows the http label on shop-web -> shop-api
    Given the "hello-shop" workspace
    When I run "stems graph --no-color"
    Then the command succeeds
    And stdout does not contain "─ http ─▶"
    When I run "stems graph --no-color --edges"
    Then the command succeeds
    And stdout contains "│ shop-web · ├─── http ─▶│ shop-api"
    When I run "stems graph --format mermaid --edges"
    Then the command succeeds
    And stdout contains "healthy http"
    When I run "stems graph --json"
    Then the JSON at "$.data.edges[?@.to=='shop-api'].protocol" equals "http"
    And the JSON at "$.data.edges[?@.from=='shop-worker' && @.to=='redis'].protocol" equals null
