@FR-LC-6
Feature: restart backoff grows exponentially
  Each consecutive restart waits `backoff.initial × factor^(attempt − 1)`,
  capped at `backoff.max` (defaults 500ms, 2, 30s): 500 ms, 1 s, 2 s. The
  delays on `stem.restarting` are computed, so they are exact; the time
  from each `stem.restarting` to the next `healthy` is measured, so it is
  only checked against a tolerant window: at least 80 % of the delay (the
  backoff is really waited), at most delay + 10 s (process start and the
  first passing probe on a loaded machine).

  Scenario: three crashes back off 500 ms, 1 s, 2 s
    Given the fixture workspace "restart"
    When I run "stems up api --detach --json"
    Then the command succeeds
    When the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s the events stream contains {"kind": "stem.restarting", "stem": "api", "data": {"attempt": 1, "delay_ms": 500}}
    And within 15s the stem "api" is "healthy"
    When the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s the events stream contains {"kind": "stem.restarting", "stem": "api", "data": {"attempt": 2, "delay_ms": 1000}}
    And within 15s the stem "api" is "healthy"
    When the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s the events stream contains {"kind": "stem.restarting", "stem": "api", "data": {"attempt": 3, "delay_ms": 2000}}
    And within 15s the stem "api" is "healthy"
    And the first event matching {"kind": "stem.restarting", "stem": "api", "data": {"attempt": 1}} is followed by one matching {"kind": "stem.state", "stem": "api", "to": "healthy"} after 400 to 10500 ms
    And the first event matching {"kind": "stem.restarting", "stem": "api", "data": {"attempt": 2}} is followed by one matching {"kind": "stem.state", "stem": "api", "to": "healthy"} after 800 to 11000 ms
    And the first event matching {"kind": "stem.restarting", "stem": "api", "data": {"attempt": 3}} is followed by one matching {"kind": "stem.state", "stem": "api", "to": "healthy"} after 1600 to 12000 ms
    And there are exactly 3 events matching {"kind": "stem.restarting", "stem": "api"}
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].restarts" equals 3
    And the JSON at "$.data.stems[0].restarts_in_window" equals 3
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
