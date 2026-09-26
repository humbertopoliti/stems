@FR-GR-1
Feature: a healthy edge waits for the dependency's real health
  `condition: healthy` (the default) means the dependency's health check
  passed, not that its process runs: with api sleeping 2 s before it listens
  (its probe fails meanwhile), web starts only after api's `healthy` event.

  Scenario: web starts after api's probe passes
    Given the fixture workspace "health-chain"
    And a local override setting stems.api.env.SHOP_SLEEP_START="2"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api", "web"]
    And the events stream contains {"kind": "stem.state", "stem": "api", "to": "healthy"} before {"kind": "stem.state", "stem": "web", "to": "starting"}
    And the events stream contains {"kind": "stem.state", "stem": "api", "to": "starting"} before {"kind": "stem.state", "stem": "api", "to": "healthy"}
    When I run "stems health api --last 50 --json"
    Then the JSON at "$.data.stems[0].results" contains {"ok": false, "outcome": "fail"}
    And the JSON at "$.data.stems[0].results" contains {"ok": true}
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
