@FR-WS-7 @NFR-3
Feature: A modified overlay is left in place
  stems only removes an overlay whose content still hashes to what it
  wrote. A file edited while the stem runs is left alone, with the warning
  event `overlay.modified_left_in_place`, and is no longer stems-owned.

  Scenario: edit the overlay while running, stop leaves it
    Given the fixture workspace "overlays"
    And the workspace has a private copy of the repos
    When I run "stems up --detach --json"
    Then the command succeeds
    Given the file "${tmp}/examples/repos/shop-api/config/local.ini" is written with "edited by hand"
    When I run "stems overlays shop-api --json"
    Then the JSON at "$.data.overlays[?@.keep==false].status" equals "modified"
    When I run "stems stop shop-api --json"
    Then the command succeeds
    And within 5s the events stream contains {"kind": "overlay.modified_left_in_place", "stem": "shop-api", "data": {"dest": "${tmp}/examples/repos/shop-api/config/local.ini"}}
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" contains "edited by hand"
    When I run "stems down --json"
    Then the command succeeds
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" contains "edited by hand"
    And no process from the workspace's process groups is alive
