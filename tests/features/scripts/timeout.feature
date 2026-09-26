@FR-SC-1 @error
Feature: script timeouts
  A script with `timeout:` that runs longer is killed together with its
  process group (children included) and fails with SCRIPT_FAILED
  (`details.reason: timeout`). The scenario runs the codebase from a private
  copy, so a leaked `sleep` (cwd inside the scenario dir) fails the leak
  check.

  Scenario: a pre_start hook that outlives its timeout
    Given the fixture workspace "scripts-timeout"
    And the workspace has a private copy of the repos
    When I run "stems up --detach --json"
    Then the exit code is 1
    And the JSON error has code "SCRIPT_FAILED"
    And the JSON at "$.errors[0].details.reason" equals "timeout"
    And the JSON at "$.errors[0].details.script" equals "pre_start"
    And the JSON at "$.errors[0].details.tail" contains "waiting"
    And the last command took less than 4000 ms
    And within 5s the events stream contains {"kind": "script.finished", "stem": "echo-svc", "data": {"script": "pre_start", "timed_out": true}}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].state" equals "failed"
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
