@docker @FR-WD-1
Feature: rebuild of a docker stem rebuilds its image and recreates it
  For a docker stem with `build:`, `action: rebuild` stops the stem (the
  container is removed) and starts it again: a fresh start always builds
  the image (`docker.build` events, same `stems/<ws>/<stem>:<run_id>` tag)
  and creates a new container. Needs Docker: `make e2e-docker`.

  Scenario: touching a file of the build context rebuilds and recreates
    Given the fixture workspace "watch-docker"
    And the workspace has a private copy of the repos
    When I run "stems up --detach --json"
    Then the command succeeds
    And within 5s the events stream contains {"kind": "docker.build", "stem": "api"}
    When I run the shell command "docker inspect -f '{{json .Id}}' watch-docker-api"
    Then the exit code is 0
    When I save the JSON at "$" as "cid"
    And I run the shell command "echo '# touched' >> ${tmp}/examples/repos/shop-api/app.py"
    Then within 10s the events stream contains {"kind": "watch.triggered", "stem": "api", "data": {"action": "rebuild"}}
    And within 180s the events stream contains {"kind": "watch.action_finished", "stem": "api", "data": {"ok": true}}
    And within 5s there are at least 2 events matching {"kind": "stem.state", "stem": "api", "to": "healthy"}
    And the events stream contains {"kind": "watch.triggered", "stem": "api"} before {"kind": "stem.state", "stem": "api", "to": "stopped", "reason": "watch rebuild"}
    And the first event matching {"kind": "watch.triggered", "stem": "api"} is followed by one matching {"kind": "docker.build", "stem": "api"} after 0 to 180000 ms
    When I run the shell command "docker inspect -f '{{json .Id}}' watch-docker-api"
    Then the exit code is 0
    And stdout does not contain "${var:cid}"
    When I run "stems down --json"
    Then the command succeeds
    And no container with label stems.workspace=watch-docker exists
    When I run the shell command "docker image ls -q stems/watch-docker/api | xargs docker image rm -f"
