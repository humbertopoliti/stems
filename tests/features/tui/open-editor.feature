@FR-UI-2
Feature: o opens the selected stem's codebase in $EDITOR
  In a terminal the dashboard suspends (alternate screen and raw mode
  off), runs `$EDITOR <codebase>` and comes back; headless runs the
  editor without touching the terminal. The fake editor records its
  argument.

  Scenario: the editor gets the codebase path; the frame is intact after
    Given the fixture workspace "run-scripts"
    And a fake tool "fake-editor" on PATH that runs "echo "$1" > ${tmp}/editor.out"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems attach --headless --script 'wait:healthy;o;frame'" with env EDITOR=fake-editor
    Then the command succeeds
    And the file "${tmp}/editor.out" contains "examples/repos/shop-api"
    And the last frame contains "[Table]"
    And the last frame contains "› shop-api"
    And the last frame contains "✓ editor closed (shop-api:"
