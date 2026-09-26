@FR-CR-1 @NFR-2 @recovery
Feature: The state file is always a complete document
  state.json is written atomically (temp file, fsync, rename), so killing
  the daemon at any point of a startup never leaves it torn; the next
  daemon adopts what was recorded and `down` cleans everything up.

  @slow
  Scenario: kill -9 the daemon repeatedly during up
    Given the fixture workspace "process-chain"
    When the daemon is killed with SIGKILL during "stems up --detach --json" once it prints "up.started", 5 times
    Then the state file is valid JSON
    # A stem spawned in the instant between fork and the state write is not
    # recorded; it is an orphan that looks like its own start command.
    When I run "stems doctor --orphans --yes --json"
    And I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.failed" equals []
    And the state file is valid JSON
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
    And the state file contains no stems
    And the lock file and socket do not exist
