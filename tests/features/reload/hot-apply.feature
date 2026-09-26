@FR-WD-3
Feature: hot changes apply without a restart
  A change to a running stem's watch rules is `watch_changed` (`hot:
  true`): the watchdog is reconfigured in place. With
  `config.reload.auto_apply: true` the daemon applies hot changes on its
  own (`config.applied {auto: true}`, `watch.reconfigured`); the stem keeps
  its process. Fixture: tests/fixtures/workspaces/watch-shop (`api` watches
  `*.py`, debounce 200ms).

  Scenario: a new debounce is applied on its own, the pid is unchanged
    Given the fixture workspace "watch-shop"
    And a local override setting config.reload.auto_apply=true
    When I run "stems up api --detach --json"
    Then the command succeeds
    And within 15s the stem "api" is "healthy"
    When I run "stems status api --json"
    And I save the JSON at "$.data.stems[0].pid" as "pid"
    Given a local override setting stems.api.watch=[{paths: ["*.py"], action: restart, debounce: 700ms}]
    Then within 5s the events stream contains {"kind": "config.changed", "data": {"plan": {"stems": [{"name": "api", "action": "watch_changed", "hot": true, "running": true}]}}}
    And within 5s the events stream contains {"kind": "watch.reconfigured", "stem": "api", "data": {"rules": [{"debounce_ms": 700}]}}
    And within 5s the events stream contains {"kind": "config.applied", "data": {"auto": true, "applied": [{"stem": "api", "action": "watch_changed", "result": "reconfigured"}]}}
    When I run "stems watch status --json"
    Then the JSON at "$.data.stems[?@.name=='api'].rules[0].debounce_ms" equals 700
    And the JSON at "$.data.stems[?@.name=='api'].active" equals true
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].pid" equals ${var:pid}
    And the events stream does not contain {"kind": "stem.state", "stem": "api", "to": "stopping"}
    When I run "stems config diff --json"
    Then the JSON at "$.data.pending" equals false
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
