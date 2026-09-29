@FR-CL-3
Feature: doctor reports where registry credentials come from
  One `docker.credentials.<registry>` result per registry an image stem
  pulls from, following the docker CLI's config ($DOCKER_CONFIG). Runs
  without Docker: the helpers are looked up, not run.

  @error
  Scenario: a credential helper that is not installed fails its registry
    Given the fixture workspace "docker-pg"
    And the file "${tmp}/dockercfg/config.json" is written with "{"credHelpers": {"https://index.docker.io/v1/": "stemstest"}}"
    When I run "stems doctor --json" with env DOCKER_HOST=unix:///nonexistent DOCKER_CONFIG=${tmp}/dockercfg
    Then the exit code is 1
    And the JSON at "$.data.checks[?@.id == 'docker.credentials.docker.io'].status" equals "fail"
    And the JSON at "$.data.checks[?@.id == 'docker.credentials.docker.io'].details.helper" equals "stemstest"
    And the JSON at "$.data.checks[?@.id == 'docker.credentials.docker.io'].details.stems" equals ["db"]
    And the JSON at "$.data.checks[?@.id == 'docker.credentials.docker.io'].hint" contains "PATH"
    And the JSON at "$.data.checks[?@.id == 'docker.credentials.docker.io'].message" contains "docker-credential-stemstest"

  Scenario: an installed credential helper is ok
    Given the fixture workspace "docker-pg"
    And the file "${tmp}/dockercfg/config.json" is written with "{"credHelpers": {"docker.io": "stemstest"}}"
    And a fake tool "docker-credential-stemstest" on PATH that runs "exit 0"
    When I run "stems doctor --json" with env DOCKER_HOST=unix:///nonexistent DOCKER_CONFIG=${tmp}/dockercfg
    Then the JSON at "$.data.checks[?@.id == 'docker.credentials.docker.io'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'docker.credentials.docker.io'].details.source" equals "helper"
    And the JSON at "$.data.checks[?@.id == 'docker.credentials.docker.io'].details.path" equals "${tmp}/bin/docker-credential-stemstest"

  Scenario: a missing credential store only warns, no config is anonymous
    Given the fixture workspace "docker-pg"
    And the file "${tmp}/dockercfg/config.json" is written with "{"credsStore": "stemsgone"}"
    When I run "stems doctor --json" with env DOCKER_HOST=unix:///nonexistent DOCKER_CONFIG=${tmp}/dockercfg
    Then the JSON at "$.data.checks[?@.id == 'docker.credentials.docker.io'].status" equals "warn"
    And the JSON at "$.data.checks[?@.id == 'docker.credentials.docker.io'].message" contains "anonymous"
    When I run "stems doctor --json" with env DOCKER_HOST=unix:///nonexistent DOCKER_CONFIG=${tmp}/nothing-here
    Then the JSON at "$.data.checks[?@.id == 'docker.credentials.docker.io'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'docker.credentials.docker.io'].details.source" equals "anonymous"
