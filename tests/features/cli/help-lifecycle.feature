@FR-SC-1
Feature: lifecycle hooks in --help
  `stems --help` names the lifecycle hooks and points at `stems scripts
  --lifecycle`; the long help of `run`, `scripts`, `build` and `reset`
  enumerates them with when each runs, generated from the same table so
  the text cannot drift. These assertions are on the help text itself.

  Scenario: stems --help and stems run --help name the hooks
    When I run "stems --help"
    Then the command succeeds
    And stdout contains "pre_start"
    And stdout contains "post_stop"
    And stdout contains "stems scripts --lifecycle"
    When I run "stems run --help"
    Then the command succeeds
    And stdout contains "pre_start"
    And stdout contains "post_stop"
    When I run "stems scripts --help"
    Then the command succeeds
    And stdout contains "--lifecycle"
    And stdout contains "bootstrap"
