@FR-WS-6
Feature: validate warns when compose stems cannot run
  With compose stems in the workspace, `docker compose` v2 is an implicit
  requirement (deliverable 15). Its absence is a `TOOL_VERSION` *warning*,
  not an error, so CI machines without Docker still validate the config.

  # stems also finds `docker` in well-known install locations (Docker
  # Desktop's app bundle, Homebrew, ...), so an empty PATH alone does not
  # hide it on a machine with Docker: STEMS_DOCKER_CLI names a missing CLI.
  Scenario: no docker on PATH is a warning, and --skip-requires skips it
    Given the fixture workspace "compose-redis"
    When I run "stems validate --json" with env PATH=${outside} STEMS_DOCKER_CLI=${outside}/docker
    Then the exit code is 0
    And the JSON at "$.data.ok" equals true
    And the JSON at "$.data.warnings[0].code" equals "TOOL_VERSION"
    And the JSON at "$.data.warnings[0].details.tool" equals "docker compose"
    And the JSON at "$.data.warnings[0].details.stems" equals ["cache"]
    When I run "stems validate --skip-requires --json" with env PATH=${outside} STEMS_DOCKER_CLI=${outside}/docker
    Then the command succeeds
    And the JSON at "$.data.warnings" equals []

  # Starting one stem of a compose project recreates the project's services
  # with that stem's `-f` set, so mixed `inject_env` is worth a warning.
  Scenario: stems of one compose project that disagree on inject_env are a warning
    Given the fixture workspace "compose-redis"
    And a local override setting stems.cache.inject_env=true
    And the local override file is extended with:
      """
      stems:
        queue:
          type: compose
          file: compose.yml
          service: redis
          ports: [{ name: redis, port: 18535, container_port: 6379 }]
      """
    When I run "stems validate --skip-requires --json"
    Then the command succeeds
    And the JSON at "$.data.warnings[0].code" equals "COMPOSE_INJECT_ENV_MIXED"
    And the JSON at "$.data.warnings[0].path" equals "stems.cache.inject_env"
    And the JSON at "$.data.warnings[0].details.project" equals "stems-compose-redis"
    And the JSON at "$.data.warnings[0].details.without" equals ["queue"]
