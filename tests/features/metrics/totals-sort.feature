@FR-MT-4
Feature: workspace totals and "what is eating my laptop"
  The human table ends with a `TOTAL` row summing CPU, memory and
  processes; `--sort mem` (or `cpu`) orders the stems heaviest first, in
  the JSON too. Values are masked in the golden (they vary run to run).

  Scenario: totals row and --sort mem on a four-stem chain
    Given the fixture workspace "process-chain"
    And a local override setting metrics.interval=300ms
    When I run "stems up --detach --json"
    Then the command succeeds
    And within 5s the JSON at "$.data.totals.children" of "stems metrics --json" equals 4
    When I run "stems metrics --human"
    Then the command succeeds
    And the output matches golden "metrics-process-chain" ignoring columns CPU%,MEM,CHILDREN,UPTIME,CPU-SPARK,MEM-SPARK
    When I run "stems metrics --sort mem --json"
    Then the command succeeds
    And the JSON at "$.data.stems" is in descending order by "latest.rss_bytes"
    And the JSON at "$.data.totals.rss_bytes" is greater than 4194304
    When I run "stems metrics --sort cpu --json"
    Then the JSON at "$.data.stems" is in descending order by "latest.cpu_pct"
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
