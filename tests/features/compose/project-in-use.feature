@docker @error @FR-ST-3
Feature: a compose project already running outside stems
  REQUIREMENTS §11 Q2: stems refuses a project that runs without it
  (COMPOSE_PROJECT_IN_USE) unless the stem says `adopt: true`, in which
  case it co-manages the project: it starts its own service there and
  `down` removes only that service. Needs Docker with compose v2:
  `make e2e-docker`.

  Scenario: refused by default, adopted with adopt: true
    Given the fixture workspace "compose-redis"
    And a local override setting stems.cache.project_name=stems-e2e-foreign
    When I run the shell command "docker compose -f compose.yml -p stems-e2e-foreign up -d other"
    Then the exit code is 0
    When I run "stems up --detach --json"
    Then the exit code is 1
    And the JSON error has code "COMPOSE_PROJECT_IN_USE"
    And the JSON at "$.errors[0].details.project" equals "stems-e2e-foreign"
    When I run "stems down --json"
    Then the command succeeds
    Given a local override setting stems.cache.adopt=true
    When I run "stems up --detach --json"
    Then the command succeeds
    And the compose project "stems-e2e-foreign" has service "redis" running
    And the compose project "stems-e2e-foreign" has service "other" running
    When I run "stems down --json"
    Then the command succeeds
    And the compose project "stems-e2e-foreign" has service "other" running
    When I run the shell command "docker compose -p stems-e2e-foreign down"
    Then the exit code is 0
