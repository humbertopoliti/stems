@FR-SC-3
Feature: scripts keep the CLI's own STEMS_* configuration
  The STEMS_* variables stems injects (STEMS_STEM, ports, ...) do not leak
  from the caller into a script, but STEMS_HOME and the other variables that
  configure the CLI pass through, so a nested `stems` command inside a script
  talks to the same daemon.

  Scenario: a script running `stems status` uses the caller's home
    Given the fixture workspace "nested-stems"
    And the stems binary is on PATH
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems run api home --json"
    Then the command succeeds
    When I run "stems logs api --script home --json"
    Then stdout contains "home=${home}"
    And stdout contains "stem=api"
    When I run "stems run api nested --json"
    Then the command succeeds
    When I run "stems down --all --json"
    Then the command succeeds
