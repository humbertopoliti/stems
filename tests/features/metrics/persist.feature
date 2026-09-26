@FR-MT-2
Feature: metrics.persist writes samples to disk
  With `metrics.persist: true` every sample is appended as one NDJSON line
  to `$STEMS_HOME/<ws>/metrics/<stem>.ndjson` (renamed
  `<stem>-YYYY-MM-DD.ndjson` when the day changes).

  Scenario: the NDJSON file grows while the stem runs
    Given the "minimal" workspace with a local override setting metrics.interval=300ms
    And a local override setting metrics.persist=true
    When I run "stems up --detach --json"
    Then the command succeeds
    And within 3s the metrics file of "echo-svc" has at least 2 lines
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
