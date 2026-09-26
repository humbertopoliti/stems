@FR-CR-1 @NFR-3 @recovery
Feature: Recovery never adopts a recycled pid
  A state entry whose pid is alive but whose start time differs belongs to
  another process now: it is cleared (`stem.recovered_dead`), never adopted
  and never signalled.

  Scenario: a state entry pointing at an unrelated process with a wrong start time
    Given the "minimal" workspace
    And a stray process "sleep 300" is running in a new process group
    And the state file records stem "echo-svc" at the stray process with a wrong start time
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["echo-svc"]
    And within 5s the events stream contains {"kind": "stem.recovered_dead", "stem": "echo-svc"}
    And the stray processes are still running
    When I run "stems down --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals ["echo-svc"]
    And the stray processes are still running
    And the state file contains no stems
    When the stray processes are stopped
    Then no process from the workspace's process groups is alive
