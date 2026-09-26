@FR-HS-4
Feature: a crossed memory limit makes the stem degraded
  `limits: { memory: 100MB }`: a sample above the limit crosses it (no
  `for` duration), emits `stem.threshold` with `state: crossed` and the
  healthy stem shows `degraded: true` with the reason `memory > 100MB`.
  Once usage drops below 90 % of the limit (10 % hysteresis) the threshold
  clears (`state: cleared`) and the stem is plainly healthy again.

  Scenario: allocating 150 MB crosses a 100 MB limit, freeing it clears
    Given the "minimal" workspace with a local override setting metrics.interval=300ms
    And a local override setting stems.echo-svc.limits.memory=100MB
    When I run "stems up --detach --json"
    Then the command succeeds
    And within 5s the JSON at "$.data.stems[0].latest.rss_bytes" of "stems metrics --json" is greater than 0
    And the JSON at "$.data.stems[0].limits[0].metric" equals "memory"
    And the JSON at "$.data.stems[0].limits[0].crossed" equals false
    When the chaos endpoint "alloc?mb=150" is called on "echo-svc"
    Then within 5s the JSON at "$.data.stems[0].degraded" of "stems status --json" equals true
    And the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].reason" contains "memory"
    And the JSON at "$.data.summary.degraded" equals 1
    And within 3s the events stream contains {"kind": "stem.threshold", "stem": "echo-svc", "data": {"metric": "memory", "state": "crossed", "limit": 104857600.0}}
    When I run "stems metrics --json"
    Then the JSON at "$.data.stems[0].limits[0].crossed" equals true
    When the chaos endpoint "alloc?mb=0" is called on "echo-svc"
    Then within 5s the JSON at "$.data.stems[0].degraded" of "stems status --json" equals false
    And within 3s the events stream contains {"kind": "stem.threshold", "stem": "echo-svc", "data": {"metric": "memory", "state": "cleared"}}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].reason" equals null
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
