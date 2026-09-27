@docker @FR-ST-3 @FR-LC-2
Feature: docker stems run as labelled containers; down removes them
  A docker stem is the container `<ws>-<stem>` carrying the labels
  `stems.workspace`, `stems.stem`, `stems.run_id` (and the user's own).
  `down` removes the container but keeps its named volumes
  (`<ws>_<name>`); `down --volumes` is destructive and needs `--yes`.
  `stop` keeps the (stopped) container; `restart` with an unchanged spec
  restarts that same container. Needs Docker: `make e2e-docker`.

  Scenario: up, down keeps the volume, down --volumes --yes removes it
    Given the fixture workspace "docker-pg"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["db"]
    And the container "docker-pg-db" is running with label "stems.workspace=docker-pg"
    And the container "docker-pg-db" is running with label "stems.stem=db"
    And the container "docker-pg-db" is running with label "team=shop"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].type" equals "docker"
    And the JSON at "$.data.stems[0].pid" equals null
    And the JSON at "$.data.stems[0].ports[0].port" equals ${port:18532}
    When I run "stems down --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals ["db"]
    And no container with label stems.workspace=docker-pg exists
    And the docker volume "docker-pg_pgdata" exists
    And the state file contains no stems
    When I run "stems down --volumes --json"
    Then the exit code is 2
    And the JSON error has code "DESTRUCTIVE_NOT_CONFIRMED"
    And the docker volume "docker-pg_pgdata" exists
    When I run "stems down --volumes --yes --json"
    Then the command succeeds
    And the JSON at "$.data.volumes_removed" equals ["docker-pg_pgdata"]
    And the docker volume "docker-pg_pgdata" does not exist
    And within 5s the lock file and socket do not exist

  Scenario: stop keeps the container, restart reuses it, down removes the leftover
    Given the fixture workspace "docker-pg"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run the shell command "docker inspect -f '{{json .Id}}' docker-pg-db"
    Then the exit code is 0
    When I save the JSON at "$" as "cid"
    And I run "stems restart db --json"
    Then the command succeeds
    And the container "docker-pg-db" is running with label "stems.stem=db"
    When I run the shell command "docker inspect -f '{{json .Id}}' docker-pg-db"
    Then the JSON at "$" equals "${var:cid}"
    When I run "stems stop db --json"
    Then the command succeeds
    When I run the shell command "docker inspect -f '{{.State.Status}}' docker-pg-db"
    Then the exit code is 0
    And stdout contains "exited"
    # Starting again replaces the stopped leftover and keeps `docker-pg_net`.
    When I run "stems start db --json"
    Then the command succeeds
    And the container "docker-pg-db" is running with label "stems.stem=db"
    When I run "stems stop db --json"
    Then the command succeeds
    When I run "stems down --volumes --yes --json"
    Then the command succeeds
    And no container with label stems.workspace=docker-pg exists
    And the docker volume "docker-pg_pgdata" does not exist
