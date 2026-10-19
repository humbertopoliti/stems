@docker @FR-ST-3 @FR-LC-6
Feature: build false with a fresh image behaves as before
  The stale guard only fires when the image is older than its inputs: a
  `build: false` stem whose image is newer than the checkout starts without
  a warning, and a crash is restarted by its policy. Needs Docker with
  compose v2: `make e2e-docker`.

  Scenario: no warning, and a crash is restarted by policy
    Given the fixture workspace "compose-build"
    And the workspace has a private copy of the repos
    And a local override setting stems.api.build=false
    When I run the shell command "printf '\n# build %s-%s\n' $(date +%s) $$ >> ${tmp}/examples/repos/shop-api/app.py"
    And I run the shell command "docker compose -f ${ws}/compose.yml -p stems-compose-build build api"
    Then the exit code is 0
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api"]
    And the JSON at "$.data.warnings" does not exist
    And the events stream does not contain {"kind": "image.stale"}
    When the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s the events stream contains {"kind": "stem.restarting", "stem": "api", "data": {"attempt": 1}}
    And within 20s the stem "api" is "healthy"
    And the events stream does not contain {"kind": "stem.gave_up"}
    When I run "stems down --json"
    Then the command succeeds
    When I run the shell command "docker compose -p stems-compose-build down"
    Then the exit code is 0
    When I run the shell command "docker image ls -q --filter label=com.docker.compose.project=stems-compose-build | xargs docker image rm -f"
