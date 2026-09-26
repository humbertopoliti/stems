@slow @NFR-1
Feature: Daemon idle footprint
  An idle daemon stays small (plan 08: < 20 MB RSS; NFR-1 allows 50 MB with
  20 stems).

  Scenario: an idle daemon uses less than 20 MB RSS
    Given the "minimal" workspace
    And the daemon is started
    Then within 5s the events stream contains {"kind": "workspace.loaded"}
    And the daemon RSS is below 20 MB
