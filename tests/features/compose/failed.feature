@docker @error @FR-ST-3
Feature: a failing docker compose invocation
  A non-zero `docker compose` exit fails the stem with COMPOSE_FAILED
  (exit 1) and the last 20 lines of compose's output in `details.tail`.
  Needs Docker with compose v2: `make e2e-docker`.

  Scenario: a service whose image does not exist
    Given the fixture workspace "compose-redis"
    And a local override setting stems.cache.service=broken
    When I run "stems up --detach --json"
    Then the exit code is 1
    And the JSON error has code "COMPOSE_FAILED"
    And the JSON at "$.errors[0].details.stem" equals "cache"
    And the JSON at "$.errors[0].details.tail" exists
    And the JSON at "$.errors[0].details.command" contains "up -d --no-deps broken"
    When I run "stems down --json"
    Then the command succeeds
    When I run the shell command "docker compose -p stems-compose-redis down"
    Then the exit code is 0
