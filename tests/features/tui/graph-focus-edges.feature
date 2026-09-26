@FR-UI-1 @FR-GR-5
Feature: Focus mode and edge labels in the graph view
  `f` shows only the selected stem and its direct neighbours (again to
  leave); `e` labels edges with their `protocol`/`via` metadata.

  Background:
    Given the fixture workspace "graph-edges"
    When I run "stems up --detach --json"
    Then the command succeeds

  Scenario: f shows only the neighbours, e shows the http label
    When I run "stems attach --headless --size 120x30 --script 'view:graph;wait:healthy;frame;f;frame;e;frame;f;frame'"
    Then the command succeeds
    And frame 1 contains "worker ✓"
    And frame 1 does not contain "http"
    And frame 2 contains "web ✓"
    And frame 2 contains "api ✓"
    And frame 2 does not contain "worker"
    And frame 2 does not contain "db ✓"
    And frame 2 contains "focus web"
    And frame 3 contains "─ http ─▶"
    And frame 4 contains "worker ✓"
    And frame 4 contains "db ✓"
