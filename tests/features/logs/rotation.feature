@slow @FR-LG-1
Feature: Log files rotate by size
  Workspace `logs: {max_size, keep}` bounds each stem's files: at most
  `keep + 1` files (`current.log`, `current.1.log`, …) exist and the newest
  lines are in `current.log`.

  Scenario: 1 MB of output with max_size 200KB and keep 2
    Given the "minimal" workspace with a local override setting logs={max_size: 200KB, keep: 2}
    When I run "stems up --detach --json"
    Then the command succeeds
    When the chaos endpoint "logs?n=8000&level=info" is called on "echo-svc"
    Then the chaos response status is 200
    And within 10s the stem log file "echo-svc/current.log" contains "INFO chaos log line 7999"
    And the stem log directory "echo-svc" holds at most 3 files
    When I run "stems logs echo-svc --json --tail 1 --grep 'chaos log line'"
    Then the JSON at "$.text" equals "INFO chaos log line 7999"
    When I run "stems down --json"
    Then the command succeeds
