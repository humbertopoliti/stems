@docker @FR-ST-3 @FR-ST-4
Feature: a key removed from an env file leaves a compose stem's env
  Env files are re-read on every env computation and `stems compose`
  regenerates the env file and the `inject_env` override whenever they
  differ, so a removed key never comes back from a file an earlier start
  wrote. The running container keeps its environment until it is
  recreated: `stems env` warns `ENV_STALE` until `stems restart`.
  Needs Docker with compose v2: `make e2e-docker`.

  Scenario: remove a key from an inject_env stem's env file, then restart
    Given the fixture workspace "compose-redis"
    And the local override file is extended with:
      """
      stems:
        cache:
          inject_env: true
          env_files: [cache.env]
      """
    When I run the shell command "printf 'GONE=1\nKEEP=2\n' > cache.env"
    Then the exit code is 0
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run the shell command "docker exec stems-compose-redis-redis-1 sh -c 'echo GONE=[$GONE] KEEP=[$KEEP]'"
    Then stdout contains "GONE=[1] KEEP=[2]"
    When I run the shell command "printf 'KEEP=2\n' > cache.env"
    Then the exit code is 0
    When I run "stems env cache --json"
    Then the JSON at "$.data.env.GONE" does not exist
    And the JSON at "$.data.env.KEEP" equals "2"
    And the JSON at "$.data.warnings[0].code" equals "ENV_STALE"
    And the JSON at "$.data.warnings[0].details.keys" equals ["GONE"]
    When I run "stems compose cache -- config"
    Then the exit code is 0
    And stdout contains "KEEP"
    And stderr contains "ENV_STALE"
    When I run "stems compose cache -- config --format json"
    Then the exit code is 0
    And the JSON at "$.services.redis.environment.GONE" does not exist
    When I run "stems restart cache --json"
    Then the command succeeds
    When I run the shell command "docker exec stems-compose-redis-redis-1 sh -c 'echo GONE=[$GONE] KEEP=[$KEEP]'"
    Then stdout contains "GONE=[] KEEP=[2]"
    When I run "stems env cache --json"
    Then the JSON at "$.data.warnings[0]" does not exist
    When I run "stems down --json"
    Then the command succeeds
    When I run the shell command "docker compose -p stems-compose-redis down"
    Then the exit code is 0
