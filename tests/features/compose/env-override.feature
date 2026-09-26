@docker @FR-ST-3 @FR-ST-4
Feature: stem env overrides a compose file's defaults
  The stem's env is exported to `docker compose` (and written to its
  `--env-file`), so it wins over `${VAR:-default}` in the compose file.
  Needs Docker with compose v2: `make e2e-docker`.

  Scenario: REDIS_ARGS from stems.local.yaml reaches the service command
    Given the fixture workspace "compose-redis"
    And a local override setting stems.cache.env.REDIS_ARGS='--maxmemory 64mb'
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run the shell command "docker inspect -f '{{json .Config.Cmd}}' stems-compose-redis-redis-1"
    Then the exit code is 0
    And stdout contains "--maxmemory"
    And stdout contains "64mb"
    When I run "stems down --json"
    Then the command succeeds
    When I run the shell command "docker compose -p stems-compose-redis down"
    Then the exit code is 0
