Feature: The terminal is restored when the dashboard panics
  The dashboard enters the alternate screen; a panic hook restores the
  terminal before the panic is reported. `STEMS_TUI_FORCE=1` opens the
  terminal dashboard even on a pipe (no raw mode, no input there) and
  `STEMS_TUI_PANIC_TEST=1` panics after the first frame.

  Scenario: a panic after the first frame still leaves the alternate screen
    Given the "minimal" workspace is up
    When I run "stems attach" with env STEMS_TUI_FORCE=1 STEMS_TUI_PANIC_TEST=1
    Then the command fails
    And stdout contains the terminal restore sequence
    And stderr contains "STEMS_TUI_PANIC_TEST"
