@docker @FR-ST-3 @FR-LC-6
Feature: a crashing compose stem with a stale image is not restarted in a loop
  With `build: false` the restart policy would start the same stale image
  again and again. Instead the crash fails the stem with `IMAGE_STALE`
  (reported like a stem that was given up on: `stem.gave_up` with reason
  `image_stale`, so the desktop notification fires), and `stems restart
  <stem> --build` rebuilds it. Needs Docker with compose v2: `make
  e2e-docker`.

  Scenario: the crash is reported once, restart --build rebuilds
    Given the fixture workspace "compose-build"
    And the workspace has a private copy of the repos
    And a local override setting stems.api.build=false
    When I run the shell command "printf '\n# build %s-%s\n' $(date +%s) $$ >> ${tmp}/examples/repos/shop-api/app.py"
    And I run the shell command "docker compose -f ${ws}/compose.yml -p stems-compose-build build api"
    Then the exit code is 0
    When I run the shell command "docker image inspect -f '{{.Id}}' stems-compose-build-api > ${tmp}/image-id-1"
    Then the exit code is 0
    When I run the shell command "touch ${tmp}/examples/repos/shop-api/app.py"
    And I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api"]
    And the JSON at "$.data.warnings[0].code" equals "IMAGE_STALE"
    When the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s the events stream contains {"kind": "stem.gave_up", "stem": "api", "data": {"reason": "image_stale", "exit_code": 3}}
    And within 5s the stem "api" is "failed"
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].error.code" equals "IMAGE_STALE"
    And the JSON at "$.data.stems[0].error.details.image" equals "stems-compose-build-api"
    And during 3s the events stream never contains {"kind": "stem.restarting"}
    And the events stream does not contain {"kind": "stem.restarting"}
    When I run the shell command "printf '\n# rebuilt %s-%s\n' $(date +%s) $$ >> ${tmp}/examples/repos/shop-api/app.py"
    And I run "stems restart api --build --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api"]
    And within 15s the stem "api" is "healthy"
    When I run the shell command "test $(docker image inspect -f '{{.Id}}' stems-compose-build-api) != $(cat ${tmp}/image-id-1)"
    Then the exit code is 0
    When I run "stems down --json"
    Then the command succeeds
    When I run the shell command "docker compose -p stems-compose-build down"
    Then the exit code is 0
    When I run the shell command "docker image ls -q --filter label=com.docker.compose.project=stems-compose-build | xargs docker image rm -f"
