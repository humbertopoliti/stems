@docker @FR-ST-3
Feature: a compose stem wraps one service of a compose file
  `stems up` runs `docker compose up -d --no-deps <service>` under the
  project `stems-<ws>` (the default), with the stem's env as interpolation
  input (REDIS_PORT publishes the remapped port); `down` removes the
  service's container. Needs Docker with compose v2: `make e2e-docker`.

  Scenario: up runs the service under stems-<ws>, down removes it
    Given the fixture workspace "compose-redis"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["cache"]
    And the compose project "stems-compose-redis" has service "redis" running
    When I run the shell command "docker port stems-compose-redis-redis-1 6379/tcp"
    Then the exit code is 0
    And stdout contains ":${port:18534}"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].type" equals "compose"
    When I run "stems down --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals ["cache"]
    And the state file contains no stems
    When I run the shell command "docker compose -p stems-compose-redis ps -aq | grep -q . && exit 1 || exit 0"
    Then the exit code is 0
    When I run the shell command "docker compose -p stems-compose-redis down"
    Then the exit code is 0
