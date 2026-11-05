@docker @FR-ST-3
Feature: stems compose runs docker compose in a compose stem's context
  `stems compose <stem> [--service] -- <args>` runs `docker compose -f <file>
  -p <project> --env-file <stem env> <args>` in your terminal, without a
  daemon. Needs Docker with compose v2: `make e2e-docker`.

  @docker
  Scenario: compose config works without a daemon
    Given the fixture workspace "compose-redis"
    When I run "stems compose cache -- config --services"
    Then the exit code is 0
    And stdout contains "redis"

  @docker
  Scenario: the stem's env reaches compose and the exit code is compose's
    Given the fixture workspace "compose-redis"
    And a local override setting stems.cache.env.REDIS_ARGS=--appendonly yes
    When I run "stems compose cache -- config"
    Then the exit code is 0
    And stdout contains "--appendonly"
    When I run "stems compose cache -- no-such-subcommand"
    Then the command fails

  @docker
  Scenario: ps lists the running container and --service appends the service
    Given the fixture workspace "compose-redis"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems compose cache -- ps --format json"
    Then the exit code is 0
    And stdout contains "stems-compose-redis-redis-1"
    When I run "stems compose cache --service -- ps --format json"
    Then the exit code is 0
    And stdout contains "stems-compose-redis-redis-1"
    When I run "stems down --json"
    Then the command succeeds
    When I run the shell command "docker compose -p stems-compose-redis down"
    Then the exit code is 0

  Scenario: a process stem is a usage error
    Given the fixture workspace "process-shop"
    When I run "stems compose shop-api -- ps"
    Then the exit code is 2
