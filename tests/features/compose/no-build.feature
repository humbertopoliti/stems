@docker @FR-ST-3
Feature: up --no-build keeps the image and warns when it is stale
  `--no-build` (or `build: false`) skips the rebuild. When the image was
  built before the service's build inputs last changed, `stems up` still
  starts the stem but reports `IMAGE_STALE` in `data.warnings` and emits
  `image.stale`. The default (`build: true`) rebuilds and never warns.
  Needs Docker with compose v2: `make e2e-docker`.

  Scenario: a touched source makes the kept image stale
    Given the fixture workspace "compose-build"
    And the workspace has a private copy of the repos
    When I run the shell command "printf '\n# build %s-%s\n' $(date +%s) $$ >> ${tmp}/examples/repos/shop-api/app.py"
    And I run "stems up --detach --json"
    Then the command succeeds
    When I run the shell command "docker image inspect -f '{{.Id}}' stems-compose-build-api > ${tmp}/image-id-1"
    Then the exit code is 0
    When I run "stems down --json"
    Then the command succeeds
    When I run the shell command "touch ${tmp}/examples/repos/shop-api/app.py"
    And I run "stems up --detach --no-build --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api"]
    And the JSON at "$.data.warnings[0].code" equals "IMAGE_STALE"
    And the JSON at "$.data.warnings[0].details.stem" equals "api"
    And the JSON at "$.data.warnings[0].details.newest_input_path" contains "shop-api/app.py"
    And within 5s the events stream contains {"kind": "image.stale", "stem": "api", "data": {"stem": "api", "image": "stems-compose-build-api"}}
    When I run the shell command "test $(docker image inspect -f '{{.Id}}' stems-compose-build-api) = $(cat ${tmp}/image-id-1)"
    Then the exit code is 0
    When I run "stems down --json"
    Then the command succeeds
    When I run the shell command "printf '\n# rebuilt %s-%s\n' $(date +%s) $$ >> ${tmp}/examples/repos/shop-api/app.py"
    And I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api"]
    And the JSON at "$.data.warnings" does not exist
    When I run the shell command "test $(docker image inspect -f '{{.Id}}' stems-compose-build-api) != $(cat ${tmp}/image-id-1)"
    Then the exit code is 0
    When I run "stems down --json"
    Then the command succeeds
    When I run the shell command "docker compose -p stems-compose-build down"
    Then the exit code is 0
    When I run the shell command "docker image ls -q --filter label=com.docker.compose.project=stems-compose-build | xargs docker image rm -f"
