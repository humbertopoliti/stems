@FR-LC-4 @FR-UI-1
Feature: Attached up opens the dashboard while the stems are starting
  On a terminal `stems up` opens the dashboard as soon as the daemon
  accepted the `up` (`up.started`), not once every stem is ready: the
  stems are watched coming up in the graph / table. The dashboard reports
  the result as a toast (from the live `up.finished` event, which only a
  dashboard opened during the `up` can see); the summary is printed when
  the dashboard closes. `STEMS_TUI_SCRIPT` drives it headless.

  Scenario: the dashboard sees the up finish, then y stops everything
    Given the fixture workspace "process-chain"
    When I run "stems up" with env STEMS_TUI_SCRIPT=wait:event=up.finished;frame;wait:healthy;q;y
    Then the exit code is 0
    And stdout contains "--- frame 1 ---"
    And stdout contains "✓ up: 4 ready"
    And within 5s the lock file and socket do not exist
    And no process from the workspace's process groups is alive

  Scenario: an up that fails as a whole closes the dashboard with its error
    Given the fixture workspace "scripts-bootstrap-fail"
    When I run "stems up" with env STEMS_TUI_SCRIPT=wait:healthy;frame
    Then the exit code is 1
    And stdout does not contain "--- frame 1 ---"
    And stdout contains "SETUP_FAILED"
    And within 5s the lock file and socket do not exist
