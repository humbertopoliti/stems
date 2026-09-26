@FR-LC-6
Feature: a stem that stays unhealthy is restarted
  With `restart.on_unhealthy: true` a stem that stays `unhealthy` for
  `restart.unhealthy_grace` is stopped and started again through the same
  path as a crash (`stem.restarting {reason: unhealthy}`, backoff, counted
  against `restart.max`). Here /healthz answers 503 for 10 s; the grace is
  1 s, so the restart comes long before the service would recover on its
  own, and the new process is healthy at once.

  Scenario: unhealthy for longer than the grace
    Given the fixture workspace "restart"
    And a local override setting stems.api.restart.on_unhealthy=true
    And a local override setting stems.api.restart.unhealthy_grace=1s
    When I run "stems up api --detach --json"
    Then the command succeeds
    When I run "stems status api --json"
    And I save the JSON at "$.data.stems[0].pid" as "pid"
    And the chaos endpoint "unhealthy?for=10s" is called on "api"
    Then the chaos response status is 200
    And within 5s the stem "api" is "unhealthy"
    And within 10s the events stream contains {"kind": "stem.restarting", "stem": "api", "data": {"reason": "unhealthy", "attempt": 1, "delay_ms": 500, "counted": true}}
    And within 15s the stem "api" is "healthy"
    And the first event matching {"kind": "stem.health", "stem": "api", "to": "unhealthy"} is followed by one matching {"kind": "stem.restarting", "stem": "api"} after 800 to 8000 ms
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].pid" does not equal ${var:pid}
    And the JSON at "$.data.stems[0].restarts" equals 1
    And within 5s none of the pids ${var:pid} is alive
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
