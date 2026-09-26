@docker @FR-CR-2 @recovery
Feature: a new daemon adopts the containers of a crashed one
  The state file records each docker stem's container id. After `kill -9`
  of the daemon, the next daemon adopts the container if it still exists,
  carries `stems.workspace`/`stems.stem` for this stem and runs, instead of
  starting a second one. Needs Docker: `make e2e-docker`.

  Scenario: up after kill -9 adopts the running container
    Given the fixture workspace "docker-pg"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run the shell command "docker inspect -f '{{json .Id}}' docker-pg-db"
    Then the exit code is 0
    When I save the JSON at "$" as "cid"
    And the daemon is killed with SIGKILL
    And I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["db"]
    And within 10s the events stream contains {"kind": "stem.adopted", "stem": "db", "data": {"container_id": "${var:cid}"}}
    And within 5s the events stream contains {"kind": "stem.state", "stem": "db", "to": "healthy", "reason": "adopted"}
    When I run the shell command "test $(docker ps -aq --filter label=stems.workspace=docker-pg | wc -l) -eq 1"
    Then the exit code is 0
    When I run the shell command "docker inspect -f '{{json .Id}}' docker-pg-db"
    Then the JSON at "$" equals "${var:cid}"
    When I run "stems down --volumes --yes --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals ["db"]
    And no container with label stems.workspace=docker-pg exists
    And the state file contains no stems
