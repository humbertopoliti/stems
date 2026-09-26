@FR-GR-6
Feature: The graph view propagates dependency health
  A stem whose hard dependency is unhealthy is degraded (`!`) even though
  its own probe passes; its box shows the reason `dep <name>` on a second
  line. It clears once the dependency recovers.

  Scenario: web shows ! and dep api while api is unhealthy, then recovers
    Given the fixture workspace "health-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    When the chaos endpoint "unhealthy?for=8s" is called on "api"
    Then within 3s the stem "api" is "unhealthy"
    When I run "stems attach --headless --size 100x30 --script 'view:graph;wait:stem=web:degraded;frame;wait:stem=api:healthy;frame'"
    Then the command succeeds
    And frame 1 contains "✗ │"
    And frame 1 does not contain "api ✓"
    And frame 1 contains "web   !"
    And frame 1 contains "dep api"
    And frame 2 contains "│ api ✓ │"
    And frame 2 contains "│ web ✓ ├"
    And frame 2 does not contain "dep api"
    When I run "stems down --json"
    Then the command succeeds
