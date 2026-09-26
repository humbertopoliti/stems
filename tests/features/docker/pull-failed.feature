@docker @error @FR-ST-3
Feature: a docker image that cannot be pulled
  Pulling a missing image fails the stem with IMAGE_PULL_FAILED (exit 1)
  carrying the registry's own message; no container is left behind.
  Needs Docker (and network access to the registry): `make e2e-docker`.

  Scenario: an image that does not exist
    Given the fixture workspace "docker-pg"
    And a local override setting stems.db.image=stems-does-not-exist:1
    When I run "stems up --detach --json"
    Then the exit code is 1
    And the JSON error has code "IMAGE_PULL_FAILED"
    And the JSON at "$.errors[0].details.stem" equals "db"
    And the JSON at "$.errors[0].details.image" equals "stems-does-not-exist:1"
    And the JSON at "$.errors[0].details.message" exists
    And the error message contains "stems-does-not-exist"
    And no container with label stems.workspace=docker-pg exists
    When I run "stems down --json"
    Then the command succeeds
