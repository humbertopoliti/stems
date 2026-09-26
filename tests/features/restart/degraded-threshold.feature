@FR-HS-4
Feature: frequent restarts degrade a stem
  Three or more policy restarts within `restart.window` make a healthy
  stem `degraded` with reason `restarts (N recently)` (glyph degraded,
  `restarts_in_window` in status). Once the restarts age out of the
  window the stem is plainly healthy again; the lifetime `restarts` count
  stays. The window is 5 s here (not the plan's 3 s): three crash/restart
  cycles must fit in it with room to spare on a loaded machine; the
  backoff is 100 ms flat so the cycles are short.

  Scenario: three restarts in a 5 s window
    Given the fixture workspace "restart"
    And a local override setting stems.api.restart.window=5s
    And a local override setting stems.api.restart.backoff.initial=100ms
    And a local override setting stems.api.restart.backoff.factor=1
    When I run "stems up api --detach --json"
    Then the command succeeds
    When the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s there are at least 1 events matching {"kind": "stem.restarting", "stem": "api"}
    And within 10s the stem "api" is "healthy"
    When the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s there are at least 2 events matching {"kind": "stem.restarting", "stem": "api"}
    And within 10s the stem "api" is "healthy"
    When the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s there are at least 3 events matching {"kind": "stem.restarting", "stem": "api"}
    And within 10s the JSON at "$.data.stems[0].degraded" of "stems status api --json" equals true
    And the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].glyph" equals "degraded"
    And the JSON at "$.data.stems[0].reason" equals "restarts (3 recently)"
    And the JSON at "$.data.stems[0].restarts_in_window" equals 3
    And the JSON at "$.data.summary.degraded" equals 1
    And within 10s the JSON at "$.data.stems[0].restarts_in_window" of "stems status api --json" equals 0
    And the JSON at "$.data.stems[0].degraded" equals false
    And the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].reason" equals null
    And the JSON at "$.data.stems[0].restarts" equals 3
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
