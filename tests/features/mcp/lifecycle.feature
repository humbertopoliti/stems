@FR-AI-1 @FR-AI-4
Feature: lifecycle through MCP, audited by actor
  Every daemon request of the MCP server carries the actor
  `mcp:<client name from initialize>`; the events it causes carry it too and
  `get_events` filters on it. Long calls (`up`, `down`, `run_script`) send
  progress notifications while they run.

  Scenario: stop, start and restart are recorded with the MCP client's actor
    Given the "minimal" workspace is up
    And an MCP client is connected
    When the MCP client calls tool "stop" with {"stems": ["echo-svc"]}
    Then the tool result is success
    And the tool result JSON at "$.stopped" equals ["echo-svc"]
    When the MCP client calls tool "start" with {"stems": ["echo-svc"]}
    Then the tool result is success
    And the tool result JSON at "$.ready" equals ["echo-svc"]
    When the MCP client calls tool "restart" with {"stems": ["echo-svc"]}
    Then the tool result is success
    And the tool result JSON at "$.ready" equals ["echo-svc"]
    And within 5s the events stream contains {"kind": "stem.state", "stem": "echo-svc", "to": "stopped", "actor": "mcp:stems-e2e"}
    And the events stream contains {"kind": "stem.state", "to": "stopping", "actor": "mcp:stems-e2e"} before {"kind": "stem.state", "to": "starting", "actor": "mcp:stems-e2e"}
    When the MCP client calls tool "get_events" with {"actor": "mcp:stems-e2e", "limit": 500}
    Then the tool result is success
    And the tool result JSON at "$.events[0].actor" equals "mcp:stems-e2e"
    And the tool result JSON at "$.events" contains {"kind": "stem.state", "stem": "echo-svc", "to": "stopped"}
    And the JSON at "$.events[?@.actor != 'mcp:stems-e2e']" does not exist
    And the tool result JSON at "$.truncated" equals false
    When the MCP client calls tool "get_events" with {"actor": "mcp:stems-e2e", "kinds": ["stem.*"], "limit": 2}
    Then the tool result is success
    And the tool result JSON at "$.returned" equals 2
    And the tool result JSON at "$.truncated" equals true
    And the JSON at "$.events[?@.kind != 'stem.state' && @.kind != 'stem.port_allocated' && @.kind != 'stem.health']" does not exist
    When I save the tool result JSON at "$.next_since_seq" as "seq"
    And the MCP client calls tool "get_events" with {"actor": "mcp:stems-e2e", "since_seq": ${var:seq}, "limit": 500}
    Then the tool result is success
    And the tool result JSON at "$.truncated" equals false
    When I run "stems down --json"
    Then the command succeeds

  Scenario: up and down through MCP stream progress and return the summary
    Given the "minimal" workspace
    And the daemon is started
    And an MCP client is connected
    When the MCP client calls tool "up" with {}
    Then the tool result is success
    And the tool result JSON at "$.ok" equals true
    And the tool result JSON at "$.ready" equals ["echo-svc"]
    And the MCP client received at least 2 progress notifications
    And within 5s the events stream contains {"kind": "up.finished", "actor": "mcp:stems-e2e"}
    When the MCP client calls tool "down" with {}
    Then the tool result is success
    And the tool result JSON at "$.stopped" equals ["echo-svc"]
    And the tool result JSON at "$.daemon_stopping" equals false
