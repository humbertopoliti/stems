@docker @FR-ST-2
Feature: docker build secrets and paths outside the context
  `build: { secrets, allow_read }` builds a docker stem with `docker buildx
  build` (BuildKit): a `{id, file}` secret is mounted by `RUN
  --mount=type=secret,id=<id>` and never becomes a build arg or a layer; a
  Dockerfile outside the context works with `allow_read`. A missing secret
  file fails the build naming the secret. Needs Docker with buildx
  (`make e2e-docker`); the `validate` rules are in
  `validate/build-secrets.feature`.

  @docker
  Scenario: the secret is mounted into the build, the Dockerfile lives outside the context
    Given the fixture workspace "docker-build-secrets"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api"]
    And within 5s the events stream contains {"kind": "docker.build", "stem": "api"}
    When I run the shell command "curl -fsS http://127.0.0.1:${port:18841}/token"
    Then the exit code is 0
    And stdout contains "s3cret-from-file"
    When I run the shell command "docker image history --no-trunc $(docker inspect -f '{{.Config.Image}}' docker-build-secrets-api) | grep -q s3cret-from-file && exit 1 || exit 0"
    Then the exit code is 0
    When I run "stems down --json"
    Then the command succeeds
    And no container with label stems.workspace=docker-build-secrets exists
    When I run the shell command "docker image ls -q stems/docker-build-secrets/api | xargs docker image rm -f"

  @docker @error
  Scenario: a missing secret file fails the build naming the secret
    Given the fixture workspace "docker-build-secrets"
    When I run the shell command "rm ${ws}/secrets/tok.txt"
    And I run "stems up --detach --json"
    Then the exit code is 1
    And the JSON error has code "SETUP_FAILED"
    And the JSON at "$.errors[0].details.stem" equals "api"
    And the JSON at "$.errors[0].details.secret" equals "tok"
    And the error message contains "tok"
    And no container with label stems.workspace=docker-build-secrets exists
    When I run "stems down --json"
    Then the command succeeds
