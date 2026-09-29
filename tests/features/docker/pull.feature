@docker @FR-ST-3
Feature: stems pull refreshes images from their registry
  A local registry stands in for GHCR/GAR: the scenario pushes an image,
  runs it, moves the tag to another build and pulls. Needs Docker:
  `make e2e-docker`.

  Scenario: a moved tag is pulled and the running stem restarted onto it
    Given the fixture workspace "docker-pull"
    When I run "stems up registry --detach --json"
    Then the command succeeds
    When I run the shell command "docker pull -q busybox:1.36 && docker tag busybox:1.36 localhost:${port:18541}/stems-e2e/app:latest && docker push -q localhost:${port:18541}/stems-e2e/app:latest"
    Then the command succeeds
    When I run "stems up app --detach --json"
    Then the command succeeds
    # Without names: `pull: never` stems are skipped.
    When I run "stems pull --json"
    Then the command succeeds
    And the JSON at "$.data.pulled[0].stem" equals "registry"
    And the JSON at "$.data.pulled[0].changed" equals false
    And the JSON at "$.data.skipped[0].stem" equals "app"
    # Move the tag in the registry (the local copy stays on 1.36).
    When I run the shell command "docker pull -q busybox:1.37 && docker tag busybox:1.37 localhost:${port:18541}/stems-e2e/app:latest && docker push -q localhost:${port:18541}/stems-e2e/app:latest && docker tag busybox:1.36 localhost:${port:18541}/stems-e2e/app:latest"
    Then the command succeeds
    When I run "stems pull app --restart --json"
    Then the command succeeds
    And the JSON at "$.data.pulled[0].stem" equals "app"
    And the JSON at "$.data.pulled[0].changed" equals true
    And the JSON at "$.data.restarted.ready" equals ["app"]
    And the events stream contains {"kind": "docker.pull", "stem": "app"} before {"kind": "stem.state", "stem": "app", "to": "stopping"}
    When I run the shell command "test $(docker inspect -f '{{.Image}}' docker-pull-app) = $(docker image inspect -f '{{.Id}}' busybox:1.37)"
    Then the command succeeds
    # Pulled again: nothing changed, nothing restarted.
    When I run "stems pull app --restart --json"
    Then the command succeeds
    And the JSON at "$.data.pulled[0].changed" equals false
    And the JSON at "$.data.restarted" does not exist
    When I run "stems down --json"
    Then the command succeeds
    And no container with label stems.workspace=docker-pull exists
    When I run the shell command "docker rmi localhost:${port:18541}/stems-e2e/app:latest"
    Then the command succeeds
