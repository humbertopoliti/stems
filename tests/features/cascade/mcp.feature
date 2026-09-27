@FR-AI-1 @FR-LC-9
Feature: the MCP restart tool can cascade
  The `restart` tool takes `cascade` like `stems restart --cascade`; the
  dependants' restarts carry the MCP client's actor.

  Scenario: restart {stems: [a], cascade: true} restarts b, c and d
    Given the fixture workspace "cascade-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    Given an MCP client is connected
    When the MCP client calls tool "restart" with {"stems": ["a"], "cascade": true}
    Then the tool result is success
    And the tool result JSON at "$.cascade.restarted" equals [["b", "c"], ["d"]]
    And the tool result JSON at "$.ready" equals ["a"]
    And within 5s the events stream contains {"kind": "cascade.finished", "actor": "mcp:stems-e2e", "data": {"restarted": ["b", "c", "d"]}}
    And the events stream contains {"kind": "cascade.started", "actor": "mcp:stems-e2e", "data": {"origin": "a"}} before {"kind": "stem.restarting", "stem": "b", "actor": "mcp:stems-e2e"}
    And there are exactly 1 events matching {"kind": "stem.restarting", "stem": "d", "actor": "mcp:stems-e2e"}
    When I run "stems down --json"
    Then the command succeeds
