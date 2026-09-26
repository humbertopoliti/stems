@FR-LC-2 @error
Feature: down --volumes is destructive and needs confirmation
  `stems down --volumes` also deletes the named volumes of the selected
  docker stems (deliverable 14), so without `--yes` (or a `y` on a
  terminal) it refuses with DESTRUCTIVE_NOT_CONFIRMED (exit 2) before
  touching anything, like `stems reset`. The Docker half is
  tests/features/docker/up-down.feature (`@docker`).

  Scenario: refused without --yes; with --yes a process-only workspace just goes down
    Given the "minimal" workspace is up in detached mode
    When I run "stems down --volumes --json"
    Then the exit code is 2
    And the JSON error has code "DESTRUCTIVE_NOT_CONFIRMED"
    And the JSON at "$.errors[0].hint" contains "--yes"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].state" equals "healthy"
    When I run "stems down --volumes --yes --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals ["echo-svc"]
    And the JSON at "$.data.volumes_removed" does not exist
    And within 2s the lock file and socket do not exist
    And no process from the workspace's process groups is alive

  Scenario: --volumes --yes without a daemon starts one briefly and leaves nothing
    Given the "minimal" workspace
    When I run "stems down --volumes --yes --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals []
    And within 2s the lock file and socket do not exist
