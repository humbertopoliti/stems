@docker @FR-MT-1
Feature: docker stems are sampled through the Docker stats API
  Container stems get one `stats(stream=false)` reading per interval: CPU %
  by Docker's formula, memory as usage minus page cache, `children` the
  container's process count.

  Scenario: postgres has memory and CPU numbers
    Given the fixture workspace "docker-pg"
    And a local override setting metrics.interval=500ms
    When I run "stems up --detach --json"
    Then the command succeeds
    And within 15s the JSON at "$.data.stems[0].latest.rss_bytes" of "stems metrics --json" is greater than 0
    And the JSON at "$.data.stems[0].latest.cpu_pct" is greater than -0.001
    And the JSON at "$.data.stems[0].latest.children" is greater than 0
    When I run "stems metrics --disk --json"
    Then the JSON at "$.data.stems[0].disk.volumes_bytes" is greater than 0
    When I run "stems down --volumes --yes --json"
    Then the command succeeds
    And no container with label stems.workspace=docker-pg exists
