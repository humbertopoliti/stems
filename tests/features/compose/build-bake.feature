@docker @FR-ST-3
Feature: compose images built with Bake (secrets, extra services, before setup)
  The object form of a compose stem's `build:` builds the listed services
  with `docker buildx bake` from the compose file plus stems' override
  (which declares the BuildKit secrets), then runs `compose up --no-build`.
  `before_setup: true` builds them before `setup`, which gets
  `STEMS_IMAGES=svc=image,...` and `STEMS_IMAGE`; the start that follows
  does not build again. Needs Docker with compose v2 and buildx:
  `make e2e-docker`.

  Scenario: both images are built with the secret before setup runs
    Given the fixture workspace "compose-build-bake"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["app"]
    And the events stream contains {"kind": "docker.build", "stem": "app"} before {"kind": "stem.state", "stem": "app", "to": "setup"}
    When I run "stems logs app --script setup --json"
    Then the JSON at "$[*].text" contains "STEMS_IMAGES=app=stems-compose-build-bake-app,migrate=stems-compose-build-bake-migrate"
    And the JSON at "$[*].text" contains "STEMS_IMAGE=stems-compose-build-bake-app"
    And the JSON at "$[*].text" contains "migrate-built"
    When I run the shell command "curl -fsS http://127.0.0.1:${port:18842}/token"
    Then the exit code is 0
    And stdout contains "bake-s3cret"
    When I run the shell command "docker run --rm stems-compose-build-bake-migrate cat /www/token"
    Then the exit code is 0
    And stdout contains "bake-s3cret"
    When I run "stems down --json"
    Then the command succeeds
    And the state file contains no stems
    When I run the shell command "docker compose -p stems-compose-build-bake down"
    Then the exit code is 0
    When I run the shell command "docker image rm -f stems-compose-build-bake-app stems-compose-build-bake-migrate"

  Scenario: without before_setup the images are built with the secret at start
    Given the fixture workspace "compose-build-bake"
    And a local override setting stems.app.build.before_setup=false
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["app"]
    And the events stream contains {"kind": "stem.state", "stem": "app", "to": "setup"} before {"kind": "docker.build", "stem": "app"}
    When I run "stems logs app --script setup --json"
    Then the JSON at "$[*].text" contains "STEMS_IMAGES="
    When I run the shell command "curl -fsS http://127.0.0.1:${port:18842}/token"
    Then the exit code is 0
    And stdout contains "bake-s3cret"
    When I run the shell command "docker image inspect stems-compose-build-bake-migrate"
    Then the exit code is 0
    When I run "stems down --json"
    Then the command succeeds
    When I run the shell command "docker compose -p stems-compose-build-bake down"
    Then the exit code is 0
    When I run the shell command "docker image rm -f stems-compose-build-bake-app stems-compose-build-bake-migrate"
