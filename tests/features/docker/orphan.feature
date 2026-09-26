@docker @FR-CR-4
Feature: containers labelled for the workspace but not in state are orphans
  `stems doctor --orphans` lists them (`kind: container`; the workspace
  label proves stems made them, so `matches_start_command` is true) and
  `--yes` removes them. `stems up` of a selection with docker stems
  refuses to start while a *running* one exists (`ORPHANS_FOUND`, exit 3)
  unless told what to do. Needs Docker: `make e2e-docker`.

  Scenario: doctor --orphans lists a labelled container and --yes removes it
    Given the fixture workspace "docker-pg"
    And a container "docker-pg-stray" is running with labels "stems.workspace=docker-pg,stems.stem=db"
    When I run "stems doctor --orphans --json"
    Then the exit code is 3
    And the JSON error has code "ORPHANS_FOUND"
    And the JSON at "$.data.orphans[0].kind" equals "container"
    And the JSON at "$.data.orphans[0].stem" equals "db"
    And the JSON at "$.data.orphans[0].matches_start_command" equals true
    And the JSON at "$.data.orphans[0].action" equals "ignored"
    When I run "stems doctor --orphans --yes --json"
    Then the command succeeds
    And the JSON at "$.data.orphans[0].action" equals "removed"
    And the JSON at "$.data.remaining" equals 0
    And no container with label stems.workspace=docker-pg exists

  Scenario: up reports a running container orphan, up --yes removes it and starts
    Given the fixture workspace "docker-pg"
    And a container "docker-pg-stray" is running with labels "stems.workspace=docker-pg,stems.stem=db"
    When I run "stems up --detach --json"
    Then the exit code is 3
    And the JSON error has code "ORPHANS_FOUND"
    And the JSON at "$.errors[0].details.orphans[0].kind" equals "container"
    When I run "stems up --detach --yes --json"
    Then the command succeeds
    And the JSON at "$.data.orphans[0].action" equals "removed"
    And the JSON at "$.data.ready" equals ["db"]
    When I run "stems down --volumes --yes --json"
    Then the command succeeds
    And no container with label stems.workspace=docker-pg exists
