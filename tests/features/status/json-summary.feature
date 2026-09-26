@FR-HS-3 @FR-HS-2
Feature: stems status --json has a summary per glyph
  `$.data.summary` counts the stems per glyph: healthy, degraded, failed,
  stopped, unknown and starting (setup/starting/seeding/stopping).

  Scenario: summary counts after stopping one stem
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status --json"
    Then the JSON at "$.data.summary" equals {"healthy": 4, "degraded": 0, "failed": 0, "stopped": 0, "unknown": 0, "starting": 0}
    When I run "stems stop d --json"
    Then the command succeeds
    When I run "stems status --json"
    Then the JSON at "$.data.summary" equals {"healthy": 3, "degraded": 0, "failed": 0, "stopped": 1, "unknown": 0, "starting": 0}
    And the JSON at "$.data.stems[?@.name=='d'].state" equals "stopped"
    And the JSON at "$.data.stems[?@.name=='d'].glyph" equals "stopped"
    And the JSON at "$.data.stems[?@.name=='d'].pid" equals null
    And the JSON at "$.data.stems[?@.name=='d'].uptime_s" equals null
    When I run "stems status d --json"
    Then the JSON at "$.data.summary" equals {"healthy": 0, "degraded": 0, "failed": 0, "stopped": 1, "unknown": 0, "starting": 0}
    When I run "stems down --all --json"
    Then the command succeeds
