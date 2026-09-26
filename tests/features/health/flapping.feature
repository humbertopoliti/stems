@FR-HS-2
Feature: a stem whose health flaps is degraded
  Three or more health transitions within 60 s make a healthy stem
  `degraded` with the reason `flapping`, even while its probe passes. The
  chaos endpoint makes api's /healthz fail for 300 ms three times; with a
  200 ms interval and retries 1 each toggle is one healthy -> unhealthy ->
  healthy round trip.

  Scenario: three quick toggles
    Given the fixture workspace "health-chain"
    When I run "stems up api --detach --json"
    Then the command succeeds
    When the chaos endpoint "unhealthy?for=300ms" is called on "api"
    Then within 3s there are at least 2 events matching {"kind": "stem.health", "stem": "api"}
    And within 2s the stem "api" is "healthy"
    When the chaos endpoint "unhealthy?for=300ms" is called on "api"
    Then within 3s there are at least 4 events matching {"kind": "stem.health", "stem": "api"}
    And within 2s the stem "api" is "healthy"
    When the chaos endpoint "unhealthy?for=300ms" is called on "api"
    Then within 3s there are at least 6 events matching {"kind": "stem.health", "stem": "api"}
    And within 2s the stem "api" is "healthy"
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].degraded" equals true
    And the JSON at "$.data.stems[0].glyph" equals "degraded"
    And the JSON at "$.data.stems[0].reason" equals "flapping"
    And the JSON at "$.data.stems[0].health.transitions_60s" is greater than 5
    And the JSON at "$.data.summary.degraded" equals 1
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
