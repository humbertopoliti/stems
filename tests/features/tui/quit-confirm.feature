@FR-LC-4
Feature: Quitting the attached dashboard
  Attached `stems up` opens the dashboard; `q` (or Ctrl-C) asks "Stop
  everything? [y/N/d(etach)]": `y` runs `down --all` and the daemon exits,
  `d` leaves everything running. `STEMS_TUI_SCRIPT` drives it headless.

  Scenario: q then y stops every stem and the daemon
    Given the "minimal" workspace
    When I run "stems up" with env STEMS_TUI_SCRIPT=wait:healthy;q;frame;y
    Then the exit code is 0
    And stdout contains "Stop everything? [y/N/d(etach)]"
    And within 5s the lock file and socket do not exist
    And no process from the workspace's process groups is alive

  Scenario: q then d detaches and leaves the daemon running
    Given the "minimal" workspace
    When I run "stems up" with env STEMS_TUI_SCRIPT=wait:healthy;q;d
    Then the exit code is 0
    And within 2s the stem "echo-svc" is "healthy"
    When I run "stems down --all --json"
    Then the command succeeds
    And within 5s the lock file and socket do not exist

  Scenario: q in a plain attach just leaves
    Given the "minimal" workspace is up
    When I run "stems attach --headless --script 'wait:healthy;q;frame'"
    Then the command succeeds
    And stdout does not contain "--- frame 1 ---"
    And within 2s the stem "echo-svc" is "healthy"
