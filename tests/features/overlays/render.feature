@FR-WS-7
Feature: Overlays are rendered on demand without starting the stem
  `stems overlays render <stem>` renders a stem's overlays now with the
  same code as a start (daemon if one runs, else locally on state.json
  under the workspace lock) and records them, so a later `down` removes
  them. `--dry-run` only prints them. Every unresolvable reference is
  listed in one UNRESOLVED_VARIABLE (exit 2).

  Scenario: dry run, render, list and down without a daemon
    Given the fixture workspace "overlays"
    And the workspace has a private copy of the repos
    When I run "stems overlays render shop-api --dry-run --json"
    Then the command succeeds
    And the JSON at "$.data.dry_run" equals true
    And the JSON at "$.data.files[0].dest" equals "${tmp}/examples/repos/shop-api/config/local.ini"
    And the JSON at "$.data.files[0].content" contains "port = ${port:18422}"
    And the JSON at "$.data.files[0].content" contains "control_port = ${port:18421}"
    And the JSON at "$.data.files[0].mode" equals "0644"
    And the JSON at "$.data.files[0].written" equals false
    And the JSON at "$.data.files[1].content" contains "kept = ${not.substituted}"
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" does not exist
    When I run "stems overlays render shop-api --json"
    Then the command succeeds
    And the JSON at "$.data.files[0].written" equals true
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" contains "port = ${port:18422}"
    When I run the shell command "ls -l ${tmp}/examples/repos/shop-api/config/local.ini"
    Then stdout contains "-rw-r--r--"
    When I run "stems overlays render shop-api --json"
    Then the command succeeds
    And the JSON at "$.data.files[0].written" equals false
    When I run "stems overlays shop-api --json"
    Then the command succeeds
    And the JSON at "$.data.overlays[?@.keep==false].status" equals "present"
    And the JSON at "$.data.overlays[?@.keep==false].dest" equals "${tmp}/examples/repos/shop-api/config/local.ini"
    When I run "stems down --json"
    Then the command succeeds
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" does not exist
    And the file "${tmp}/examples/repos/shop-api/config/keep.ini" exists
    And no process from the workspace's process groups is alive

  Scenario: render through the running daemon, removed by down
    Given the fixture workspace "overlays"
    And the workspace has a private copy of the repos
    When I run "stems up shop-worker --detach --json"
    Then the command succeeds
    When I run "stems overlays render shop-api --json"
    Then the command succeeds
    And the JSON at "$.data.files[0].written" equals true
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" contains "control_port = ${port:18421}"
    And within 5s the events stream contains {"kind": "overlay.materialised", "stem": "shop-api"}
    When I run "stems down --json"
    Then the command succeeds
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" does not exist
    And no process from the workspace's process groups is alive

  @error
  Scenario: every unresolved reference is listed
    Given the fixture workspace "overlays"
    And the workspace has a private copy of the repos
    And the file "overlays/shop-api/local.ini.tmpl" is written with "a = ${env.STEMS_E2E_NOPE_A}\nb = ${var.nope_b}"
    When I run "stems overlays render shop-api --json"
    Then the exit code is 2
    And the JSON error has code "UNRESOLVED_VARIABLE"
    And the error message contains "STEMS_E2E_NOPE_A"
    And the error message contains "nope_b"
    And the file "${tmp}/examples/repos/shop-api/config/local.ini" does not exist
    And the file "${tmp}/examples/repos/shop-api/config/keep.ini" does not exist

  @error
  Scenario: an unknown stem
    Given the fixture workspace "overlays"
    When I run "stems overlays render nope --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_STEM"
