@FR-GR-2 @FR-GR-1
Feature: Start order follows the dependency graph
  Stems start layer by layer (a topological sort): a dependant starts only
  once its hard dependencies reached the edge condition (`healthy` by
  default), independent stems start in parallel, and `down` stops in the
  reverse order. The `process-chain` fixture is a diamond (b and c depend on
  a, d depends on b and c) whose stems sleep 0.5 s before listening.

  Scenario: a diamond starts in layers with the middle layer in parallel
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["a", "b", "c", "d"]
    And the events stream contains {"kind": "up.started", "data": {"layers": [["a"], ["b", "c"], ["d"]]}} before {"kind": "stem.state", "stem": "a", "to": "starting"}
    # dependants wait for `healthy`
    And the events stream contains {"stem": "a", "to": "healthy"} before {"stem": "b", "to": "starting"}
    And the events stream contains {"stem": "a", "to": "healthy"} before {"stem": "c", "to": "starting"}
    And the events stream contains {"stem": "b", "to": "healthy"} before {"stem": "d", "to": "starting"}
    And the events stream contains {"stem": "c", "to": "healthy"} before {"stem": "d", "to": "starting"}
    # b and c overlap: each starts before the other is healthy
    And the events stream contains {"stem": "b", "to": "starting"} before {"stem": "c", "to": "healthy"}
    And the events stream contains {"stem": "c", "to": "starting"} before {"stem": "b", "to": "healthy"}
    When I run "stems events -f --json" in the background
    And I run "stems down --all --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" contains "d"
    # reverse order: d, then b and c, then a
    And within 5s the background command's output contains [{"stem": "d", "to": "stopped"}, {"stem": "b", "to": "stopping"}, {"stem": "a", "to": "stopping"}, {"stem": "a", "to": "stopped"}] in order
    And within 5s the background command's output contains [{"stem": "c", "to": "stopped"}, {"stem": "a", "to": "stopping"}] in order
    And within 5s the background command exits with code 0
    And no process from the workspace's process groups is alive
