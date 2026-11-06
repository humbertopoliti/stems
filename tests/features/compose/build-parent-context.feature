@docker @FR-ST-3
Feature: a compose build context in a parent directory needs no allow_read
  Bake checks `fs.read` relative to the compose file's directory. The
  workspace root is granted by default, so `context: ..` (a parent of
  `deploy/compose.yml`) builds without any `allow_read` entry. Needs Docker
  with compose v2 and buildx: `make e2e-docker`.

  Scenario: a parent-directory context builds with the default grant
    Given the fixture workspace "compose-build-parent"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["app"]
    When I run the shell command "curl -fsS http://127.0.0.1:${port:18843}/index.txt"
    Then the exit code is 0
    And stdout contains "parent-context-ok"
    When I run "stems down --json"
    Then the command succeeds
    When I run the shell command "docker compose -p stems-compose-build-parent down"
    Then the exit code is 0
    When I run the shell command "docker image rm -f stems-compose-build-parent-app"
