@docker @FR-ST-3 @FR-ST-4
Feature: compose inject_env puts the stem's env into the container
  Stem env is only compose's interpolation input: a compose file that does
  not name `FOO` gives the container no `FOO`. With `inject_env: true`,
  stems passes a second `-f` override that adds the stem's env file to the
  service's `env_file:`, and `PORT` is the container port.
  Needs Docker with compose v2: `make e2e-docker`.

  Scenario: without inject_env the container does not see FOO
    Given the fixture workspace "compose-redis"
    And a local override setting stems.cache.env.FOO=bar
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run the shell command "docker exec stems-compose-redis-redis-1 sh -c 'echo FOO=[$FOO]'"
    Then the exit code is 0
    And stdout contains "FOO=[]"
    When I run "stems down --json"
    Then the command succeeds
    When I run the shell command "docker compose -p stems-compose-redis down"
    Then the exit code is 0

  Scenario: with inject_env the container gets the stem env and its own PORT
    Given the fixture workspace "compose-redis"
    And a local override setting stems.cache.env.FOO=bar
    And a local override setting stems.cache.inject_env=true
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run the shell command "docker exec stems-compose-redis-redis-1 sh -c 'echo FOO=[$FOO] PORT=[$PORT] STEM=[$STEMS_STEM]'"
    Then the exit code is 0
    And stdout contains "FOO=[bar]"
    And stdout contains "PORT=[6379]"
    And stdout contains "STEM=[cache]"
    When I run "stems down --json"
    Then the command succeeds
    When I run the shell command "docker compose -p stems-compose-redis down"
    Then the exit code is 0
