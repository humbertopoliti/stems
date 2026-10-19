@docker @FR-ST-3
Feature: a compose stem builds its service by default
  A compose stem's `build` setting defaults to true: `stems up` runs
  `docker compose up -d --no-deps --build <service>`, streaming the build
  output as `docker.build` events, and `stems restart` rebuilds the image
  from the checkout (a changed source produces a new image id). Needs
  Docker with compose v2: `make e2e-docker`.

  Scenario: up builds the image and restart rebuilds it after a source change
    Given the fixture workspace "compose-build"
    And the workspace has a private copy of the repos
    When I run the shell command "printf '\n# build %s-%s\n' $(date +%s) $$ >> ${tmp}/examples/repos/shop-api/app.py"
    And I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api"]
    And the JSON at "$.data.warnings" does not exist
    And within 5s the events stream contains {"kind": "docker.build", "stem": "api"}
    And the compose project "stems-compose-build" has service "api" running
    When I run the shell command "docker image inspect -f '{{.Id}}' stems-compose-build-api > ${tmp}/image-id-1"
    Then the exit code is 0
    When I run the shell command "printf '\n# rebuilt %s-%s\n' $(date +%s) $$ >> ${tmp}/examples/repos/shop-api/app.py"
    And I run "stems restart api --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api"]
    When I run the shell command "test $(docker image inspect -f '{{.Id}}' stems-compose-build-api) != $(cat ${tmp}/image-id-1)"
    Then the exit code is 0
    When I run "stems down --json"
    Then the command succeeds
    And the state file contains no stems
    When I run the shell command "docker compose -p stems-compose-build down"
    Then the exit code is 0
    When I run the shell command "docker image ls -q --filter label=com.docker.compose.project=stems-compose-build | xargs docker image rm -f"
