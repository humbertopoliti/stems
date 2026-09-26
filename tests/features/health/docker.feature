@docker @FR-HS-1
Feature: docker probes use the container's own healthcheck
  A `docker` probe passes when Docker reports the container `healthy` (its
  healthcheck; `starting` and `unhealthy` fail); a container without a
  healthcheck passes while it runs. The fixture gives postgres a
  `pg_isready` healthcheck.

  Scenario: postgres is healthy once pg_isready passes
    Given the fixture workspace "health-docker"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].health.type" equals "docker"
    And the JSON at "$.data.stems[0].health.last.detail" equals "container healthy"
    When I run "stems health db --last 50 --json"
    Then the JSON at "$.data.stems[0].results" contains {"ok": true, "detail": "container healthy"}
    When I run "stems down --json"
    Then the command succeeds
    And no container with label stems.workspace=health-docker exists
