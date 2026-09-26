@docker @FR-ST-3 @FR-ST-4
Feature: a docker stem's env and ports reach its container
  The container gets the env stems resolves for the stem (config, local
  overrides, `STEMS_*`; never the daemon's own environment), with `PORT`
  set to the primary *container* port, and publishes each declared host
  port (here the harness-remapped one) on its `container_port`. Needs
  Docker: `make e2e-docker`.

  Scenario: a local env override and the remapped host port
    Given the fixture workspace "docker-pg"
    And a local override setting stems.db.env.POSTGRES_PASSWORD=from-local
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run the shell command "docker inspect -f '{{range .Config.Env}}{{println .}}{{end}}' docker-pg-db"
    Then the exit code is 0
    And stdout contains "POSTGRES_PASSWORD=from-local"
    And stdout contains "STEMS_STEM=db"
    And stdout contains "PORT=5432"
    When I run the shell command "docker port docker-pg-db 5432/tcp"
    Then the exit code is 0
    And stdout contains ":${port:18532}"
    When I run "stems down --volumes --yes --json"
    Then the command succeeds
    And no container with label stems.workspace=docker-pg exists
    And the docker volume "docker-pg_pgdata" does not exist
