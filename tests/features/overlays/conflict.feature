@FR-WS-7 @NFR-3 @error
Feature: Overlays never clobber a file stems did not write
  A destination that exists and is not stems-owned is `OVERLAY_CONFLICT`:
  exit 2 from `stems validate` (the `broken/overlay-conflict` row of
  tests/features/validate/broken.feature covers the example workspace), and
  a failed stem (exit 1, or 3 when others came up) from `up`.
  `up --force-overlays` backs the file up under STEMS_HOME
  (`<ws-hash>/stems/<stem>/overlay-backups/<run_id>/…`) and proceeds.

  Background:
    Given the fixture workspace "overlays"
    And the workspace has a private copy of the repos
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" is written with "mine"

  Scenario: validate reports the conflict
    When I run "stems validate --json"
    Then the exit code is 2
    And the JSON error has code "OVERLAY_CONFLICT" and path "stems.shop-api.overlays"

  Scenario: up refuses, --force-overlays backs up and proceeds
    When I run "stems up --detach --json"
    Then the exit code is 3
    And the JSON error has code "OVERLAY_CONFLICT"
    And the JSON at "$.errors[0].hint" contains "keep: true and an explicit .gitignore"
    And the JSON at "$.data.failed[0].stem" equals "shop-api"
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" contains "mine"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='shop-api'].state" equals "failed"
    When I run "stems down --json"
    Then the command succeeds
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" contains "mine"
    When I run "stems up --detach --force-overlays --json"
    Then the command succeeds
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" contains "port = ${port:18422}"
    When I run "stems events --json --since 0"
    And I save the JSON at "$[?@.kind=='overlay.materialised' && @.data.backup != null].data.backup" as "backup"
    Then the file "${var:backup}" contains "mine"
    And the JSON at "$[?@.kind=='overlay.materialised' && @.data.backup != null].data.backup" contains "${home}"
    When I run "stems down --json"
    Then the command succeeds
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" does not exist
    And no process from the workspace's process groups is alive
