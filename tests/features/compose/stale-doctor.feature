@docker @FR-ST-3
Feature: doctor reports an image older than the checkout
  `stems doctor` has an `image.<stem>` check for every compose stem whose
  service has `build:`: it compares the image's creation time with the
  newest modification time among the build inputs (the context, honouring
  `.dockerignore`, the Dockerfile and the compose file). `up --build`
  (or `restart <stem> --build` of a running stem) rebuilds it. Needs
  Docker with compose v2: `make e2e-docker`.

  Scenario: a touched source warns until the image is rebuilt
    Given the fixture workspace "compose-build"
    And the workspace has a private copy of the repos
    And a local override setting stems.api.build=false
    When I run the shell command "printf '\n# build %s-%s\n' $(date +%s) $$ >> ${tmp}/examples/repos/shop-api/app.py"
    And I run the shell command "docker compose -f ${ws}/compose.yml -p stems-compose-build build api"
    Then the exit code is 0
    When I run the shell command "touch ${tmp}/examples/repos/shop-api/app.py"
    And I run "stems doctor --json"
    Then the JSON at "$.data.checks[?@.id == 'image.api'].status" equals "warn"
    And the JSON at "$.data.checks[?@.id == 'image.api'].details.newest_input_path" contains "shop-api/app.py"
    And the JSON at "$.data.checks[?@.id == 'image.api'].details.image" equals "stems-compose-build-api"
    When I run the shell command "printf '\n# rebuilt %s-%s\n' $(date +%s) $$ >> ${tmp}/examples/repos/shop-api/app.py"
    And I run "stems up --detach --build --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api"]
    And the JSON at "$.data.warnings" does not exist
    When I run "stems doctor --json"
    Then the JSON at "$.data.checks[?@.id == 'image.api'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'image.api'].details.image" equals "stems-compose-build-api"
    When I run "stems down --json"
    Then the command succeeds
    When I run the shell command "docker compose -p stems-compose-build down"
    Then the exit code is 0
    When I run the shell command "docker image ls -q --filter label=com.docker.compose.project=stems-compose-build | xargs docker image rm -f"
