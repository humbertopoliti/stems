@FR-HS-3
Feature: stems status prints a table for humans
  Columns `STEM TYPE STATUS REASON PID PORTS UPTIME RESTARTS`; STATUS is
  the glyph and the state. The harness sets STEMS_NO_COLOR, so glyphs use
  the ASCII fallback (`OK`, `-`, `?`, ...). PID and UPTIME vary per run.

  Scenario: status of the running minimal workspace matches the golden
    Given the "minimal" workspace is up
    When I run "stems status --human"
    Then the command succeeds
    And the output matches golden "status-minimal" ignoring columns PID,UPTIME

  Scenario: status of selected stems only, and an unknown stem
    Given the fixture workspace "process-chain"
    When I run "stems up a --detach --json"
    Then the command succeeds
    When I run "stems status a b --human"
    Then the command succeeds
    And stdout contains "OK healthy"
    And stdout contains "- stopped"
    And stdout contains "1 healthy, 0 degraded, 0 failed, 0 starting, 1 stopped, 0 unknown"
    When I run "stems status nope --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_STEM"
    When I run "stems down --all --json"
    Then the command succeeds
