@FR-CR-4 @recovery
Feature: A stem's child in its own process group is not an orphan
  A dev server started by a launcher script may move into its own process
  group (setsid). The orphan scan walks a listener's parent chain up to pid
  1: when it reaches a recorded stem's pid or process group, the listener is
  that stem's child, never an orphan, so `up` does not report it and
  `doctor --orphans --yes` does not kill it.

  Scenario: up of another stem and doctor --orphans leave the child alone
    Given the fixture workspace "setsid-child"
    When I run "stems up web --detach --json"
    Then the command succeeds
    And within 10s port ${port:18611} is listening
    When I run "stems up worker --detach --json"
    Then the command succeeds
    When I run "stems doctor --orphans --yes --json"
    Then the JSON at "$.data.orphans" equals []
    When I run "stems up --detach --kill-orphans --json"
    Then the command succeeds
    And within 1s port ${port:18611} is listening
    And within 5s the stem "web" is "healthy"
    When I run "stems down --json"
    Then the command succeeds
    And within 5s the lock file and socket do not exist
    And no process from the workspace's process groups is alive
