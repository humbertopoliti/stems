@FR-SC-6
Feature: a script that must only run while some stems are stopped
  `requires_stopped: [stems]` (a sibling of `requires:`) refuses to run the
  script while any listed stem runs (`SCRIPT_REQUIRES_UNMET` with
  `details.unmet_stopped`); `--stop-deps` stops them first, the mirror of
  `--start-deps`. `stems status --expect <state> [stems]` checks the
  outcome without parsing `status --json` (deliverable 49).

  Scenario: refused while api runs; --stop-deps stops it and runs
    Given the fixture workspace "script-preconditions"
    When I run "stems up api --detach --json"
    Then the command succeeds
    When I run "stems run --ws migrate --json"
    Then the exit code is 1
    And the JSON error has code "SCRIPT_REQUIRES_UNMET"
    And the JSON at "$.errors[0].details.unmet_stopped[0].stem" equals "api"
    And the JSON at "$.errors[0].details.unmet_stopped[0].state" equals "healthy"
    And the JSON at "$.errors[0].details.requires_stopped" equals ["api"]
    And the JSON at "$.errors[0].hint" contains "--stop-deps"
    And the events stream does not contain {"kind": "script.started"}
    When I run "stems scripts --json"
    Then the JSON at "$.data.scripts[?@.name == 'migrate'].requires_stopped" equals ["api"]
    When I run "stems run --ws migrate --stop-deps --json"
    Then the command succeeds
    And the JSON at "$.data.tail[0]" equals "migrated"
    And the events stream contains {"kind": "stem.state", "stem": "api", "to": "stopped"} before {"kind": "script.started", "data": {"script": "migrate"}}
    When I run "stems status --expect stopped api --json"
    Then the command succeeds
    And the JSON at "$.data.mismatch" equals []
    When I run "stems status --expect healthy api --json"
    Then the exit code is 1
    And the JSON error has code "STATUS_MISMATCH"
    And the JSON at "$.data.mismatch[0].stem" equals "api"
    And the JSON at "$.data.mismatch[0].state" equals "stopped"
    When I run "stems down --all --json"
    Then the command succeeds

  Scenario: a stopped stem meets requires_stopped; without a daemon every stem is stopped
    Given the fixture workspace "script-preconditions"
    When I run "stems status --expect stopped --json"
    Then the command succeeds
    And the JSON at "$.data.mismatch" equals []
    And the JSON at "$.data.stems[?@.name == 'api'].state" equals "stopped"
    When I run "stems status --expect healthy api --json"
    Then the exit code is 4
    And the JSON error has code "DAEMON_NOT_RUNNING"
    When I run "stems run --ws migrate --json"
    Then the command succeeds
    And the JSON at "$.data.tail[0]" equals "migrated"

  @error
  Scenario: --start-deps that starts a requires_stopped stem again is refused
    Given the fixture workspace "script-preconditions"
    When I run "stems run --ws conflict --start-deps --json"
    Then the exit code is 1
    And the JSON error has code "SCRIPT_REQUIRES_UNMET"
    And the JSON at "$.errors[0].details.unmet_stopped[0].stem" equals "api"
    And the JSON at "$.errors[0].hint" contains "started them again"
    And the events stream does not contain {"kind": "script.started", "data": {"script": "conflict"}}
    When I run "stems down --all --json"
    Then the command succeeds
