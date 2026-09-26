@FR-WS-7 @FR-WS-2
Feature: Overlays are materialised into the codebase and removed on down
  A stem's `overlays:` are written into its codebase after `setup` and
  before `pre_start` (templates rendered with the same `${…}` substitution
  as the config, including `${stem.self.port}`), recorded in state.json
  before they are written, and removed again on `down` when unchanged. The
  codebase is otherwise never touched (FR-WS-2).

  Scenario: the rendered overlay exists while running and is gone after down
    Given the fixture workspace "overlays"
    And the workspace has a private copy of the repos
    When I run "stems up --detach --json"
    Then the command succeeds
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" contains "port = ${port:18422}"
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" contains "control_port = ${port:18421}"
    And the file "${tmp}/examples/repos/shop-api/config/keep.ini" contains "kept = ${not.substituted}"
    And within 5s the stem log file "shop-api/current.log" contains "config: port = ${port:18422}"
    And the events stream contains {"kind": "overlay.materialised", "stem": "shop-api"} before {"kind": "stem.state", "stem": "shop-api", "to": "starting"}
    When I run "stems overlays shop-api --json"
    Then the command succeeds
    And the JSON at "$.data.overlays[?@.keep==false].status" equals "present"
    And the JSON at "$.data.overlays[?@.keep==false].dest" equals "${tmp}/examples/repos/shop-api/config/local.ini"
    When I run "stems down --json"
    Then the command succeeds
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" does not exist
    And no process from the workspace's process groups is alive
