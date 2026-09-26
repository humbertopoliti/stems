@FR-WS-7
Feature: keep: true overlays survive down
  An overlay with `keep: true` stays in the codebase after `down` and stays
  recorded, so `stems overlays` (daemonless) still lists it and the next
  `up` does not see it as a conflict.

  Scenario: the kept overlay is present after down and reused by the next up
    Given the fixture workspace "overlays"
    And the workspace has a private copy of the repos
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems down --json"
    Then the command succeeds
    And the file "${tmp}/examples/repos/shop-api/config/keep.ini" exists
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" does not exist
    When I run "stems overlays --json"
    Then the command succeeds
    And the JSON at "$.data.overlays[*].dest" equals "${tmp}/examples/repos/shop-api/config/keep.ini"
    And the JSON at "$.data.overlays[0].status" equals "present"
    And the JSON at "$.data.overlays[0].keep" equals true
    When I run "stems validate --json"
    Then the command succeeds
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems down --json"
    Then the command succeeds
    And the file "${tmp}/examples/repos/shop-api/config/keep.ini" exists
    And no process from the workspace's process groups is alive
