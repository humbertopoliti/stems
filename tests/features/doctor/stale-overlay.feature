@FR-CL-3
Feature: doctor finds and removes stale overlays
  The state file's overlay ledger records a file stems wrote into a
  codebase. While its stem is not running that record is stale (a crash, or
  a daemon killed before cleanup); `--fix` deletes the unchanged file and
  forgets the record.

  Scenario: a recorded overlay of a stopped stem
    Given the fixture workspace "overlays"
    And the workspace has a private copy of the repos
    And the state file records an overlay for stem "shop-api" at "${tmp}/examples/repos/shop-api/config/local.ini"
    When I run "stems doctor --json"
    Then the command succeeds
    And the JSON at "$.data.checks[?@.id == 'overlays.shop-api'].status" equals "warn"
    And the JSON at "$.data.checks[?@.id == 'overlays.shop-api'].fixable" equals true
    And the JSON at "$.data.checks[?@.id == 'overlays.shop-api'].message" contains "stale overlay"
    And the JSON at "$.data.checks[?@.id == 'overlays.shop-api'].details.overlays[0].status" equals "present"
    When I run "stems doctor --fix --yes --json"
    Then the command succeeds
    And the JSON at "$.data.fixed[0].id" equals "overlays.shop-api"
    And the JSON at "$.data.fixed[0].action" equals "remove_overlay"
    And the JSON at "$.data.fixed[0].ok" equals true
    And the JSON at "$.data.checks[?@.id == 'overlays.shop-api']" does not exist
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" does not exist
    And the lock file and socket do not exist
    When I run "stems overlays --json"
    Then the command succeeds
    And the JSON at "$.data.overlays" equals []

  Scenario: a modified stale overlay is left in place
    Given the fixture workspace "overlays"
    And the workspace has a private copy of the repos
    And the state file records an overlay for stem "shop-api" at "${tmp}/examples/repos/shop-api/config/local.ini"
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" is written with "edited by hand"
    When I run "stems doctor --fix --yes --json"
    Then the command succeeds
    And the JSON at "$.data.fixed[0].action" equals "leave_modified_overlay"
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" contains "edited by hand"
    And the JSON at "$.data.checks[?@.id == 'overlays.shop-api']" does not exist
