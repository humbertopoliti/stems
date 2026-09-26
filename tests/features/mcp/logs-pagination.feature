@FR-AI-1
Feature: get_logs is paginated
  `get_logs` returns at most 500 records per call, oldest first, with an
  opaque `next_cursor` that carries the position and the filters;
  `truncated` is true when the result does not hold every matching record.

  Scenario: 1200 lines come back in three pages, each line exactly once
    Given the "minimal" workspace is up
    When the chaos endpoint "logs?n=1200&level=info" is called on "echo-svc"
    Then the chaos response status is 200
    And within 5s the stem log file "echo-svc/current.log" contains "chaos log line 1199"
    Given an MCP client is connected
    When the MCP client calls tool "get_logs" with {"stems": ["echo-svc"], "grep": "chaos log line", "since": "10m"}
    Then the tool result is success
    And the tool result JSON at "$.returned" equals 500
    And the tool result JSON at "$.truncated" equals true
    And the tool result JSON at "$.records[0].text" contains "chaos log line 0"
    When I save the tool result JSON at "$.next_cursor" as "c1"
    And the MCP client calls tool "get_logs" with {"cursor": "${var:c1}"}
    Then the tool result is success
    And the tool result JSON at "$.returned" equals 500
    And the tool result JSON at "$.truncated" equals true
    And the tool result JSON at "$.records[0].text" contains "chaos log line 500"
    When I save the tool result JSON at "$.next_cursor" as "c2"
    And the MCP client calls tool "get_logs" with {"cursor": "${var:c2}"}
    Then the tool result is success
    And the tool result JSON at "$.returned" equals 200
    And the tool result JSON at "$.truncated" equals false
    And the tool result JSON at "$.records[199].text" contains "chaos log line 1199"
    And across the last 3 tool results the values at "$.records[*].text" are 1200 distinct of 1200
    When I save the tool result JSON at "$.next_cursor" as "c3"
    And the MCP client calls tool "get_logs" with {"cursor": "${var:c3}"}
    Then the tool result is success
    And the tool result JSON at "$.returned" equals 0
    And the tool result JSON at "$.truncated" equals false
    When the MCP client calls tool "get_logs" with {"stems": ["echo-svc"], "grep": "chaos log line"}
    Then the tool result is success
    And the tool result JSON at "$.returned" equals 500
    And the tool result JSON at "$.truncated" equals true
    And the tool result JSON at "$.records[499].text" contains "chaos log line 1199"
    When the MCP client calls tool "get_logs" with {"stems": ["echo-svc"], "grep": "chaos log line", "tail": 3, "limit": 2}
    Then the tool result JSON at "$.returned" equals 2
    And the tool result JSON at "$.truncated" equals true
    And the tool result JSON at "$.records[0].text" contains "chaos log line 1197"
    When I run "stems down --json"
    Then the command succeeds

  @error
  Scenario: a bad cursor is a usage error
    Given the "minimal" workspace is up
    And an MCP client is connected
    When the MCP client calls tool "get_logs" with {"cursor": "garbage"}
    Then the tool result is error
    And the tool result JSON at "$.code" equals "USAGE"
    When I run "stems down --json"
    Then the command succeeds
