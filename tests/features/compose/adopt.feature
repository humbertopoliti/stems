@docker @FR-CR-2 @recovery
Feature: a new daemon adopts a compose stem's container
  The recorded container id must still exist, carry the compose labels of
  the stem's project and service, and run. Needs Docker with compose v2:
  `make e2e-docker`.

  Scenario: up after kill -9 adopts the compose container
    Given the fixture workspace "compose-redis"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run the shell command "docker inspect -f '{{json .Id}}' stems-compose-redis-redis-1"
    Then the exit code is 0
    When I save the JSON at "$" as "cid"
    And the daemon is killed with SIGKILL
    And I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["cache"]
    And within 10s the events stream contains {"kind": "stem.adopted", "stem": "cache", "data": {"container_id": "${var:cid}"}}
    When I run the shell command "docker inspect -f '{{json .Id}}' stems-compose-redis-redis-1"
    Then the JSON at "$" equals "${var:cid}"
    When I run "stems down --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals ["cache"]
    And the state file contains no stems
    When I run the shell command "docker compose -p stems-compose-redis down"
    Then the exit code is 0
