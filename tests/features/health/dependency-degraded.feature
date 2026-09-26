@FR-GR-6 @FR-HS-2
Feature: a healthy stem with an unhealthy dependency is degraded
  Degraded (glyph `!`) is derived for every `status`, never stored: a stem
  whose own probe passes but whose hard dependency is `unhealthy` or
  `failed` shows `degraded: true` with the reason `dependency <name>
  unhealthy`, and is counted in `summary.degraded`. It clears as soon as the
  dependency recovers.

  Scenario: web is degraded while api is unhealthy
    Given the fixture workspace "health-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api", "web"]
    When the chaos endpoint "unhealthy?for=3s" is called on "api"
    Then within 2s the stem "api" is "unhealthy"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='web'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='web'].degraded" equals true
    And the JSON at "$.data.stems[?@.name=='web'].glyph" equals "degraded"
    And the JSON at "$.data.stems[?@.name=='web'].reason" equals "dependency api unhealthy"
    And the JSON at "$.data.summary.degraded" equals 1
    And the JSON at "$.data.summary.failed" equals 1
    When I run "stems status web --human"
    Then stdout contains "dependency api unhealthy"
    Then within 5s the stem "api" is "healthy"
    And within 2s the JSON at "$.data.stems[?@.name=='web'].degraded" of "stems status --json" equals false
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='web'].reason" equals null
    And the JSON at "$.data.summary.degraded" equals 0
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
