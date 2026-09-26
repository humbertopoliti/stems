@FR-WS-7 @FR-CR-3 @recovery
Feature: Overlays are cleaned up after a daemon crash
  Overlay records live in state.json (written before the file), so after
  `kill -9` of the daemon, `stems down` — which starts a new daemon that
  adopts the running stems — still removes them.

  Scenario: kill -9 on the daemon, then down removes the overlay
    Given the fixture workspace "overlays"
    And the workspace has a private copy of the repos
    When I run "stems up --detach --json"
    Then the command succeeds
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" exists
    When the daemon is killed with SIGKILL
    And I run "stems overlays --json"
    Then the command succeeds
    And the JSON at "$.data.overlays[?@.keep==false].status" equals "present"
    When I run "stems down --json"
    Then the command succeeds
    And the JSON at "$.data.recovered" equals true
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" does not exist
    And the file "${tmp}/examples/repos/shop-api/config/keep.ini" exists
    And no process from the workspace's process groups is alive
    And the state file contains no stems
