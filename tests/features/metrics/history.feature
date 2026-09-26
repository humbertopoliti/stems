@FR-MT-2
Feature: the session history is kept in memory and sliced by --history
  The daemon keeps 30 minutes of samples per stem. `--history <window>`
  adds the samples of that window (oldest first) to the JSON; without it
  only `latest` is returned. At 300 ms a 10 s window holds at most ~34
  samples; the bounds are loose.

  Scenario: --history returns the samples of the window
    Given the "minimal" workspace with a local override setting metrics.interval=300ms
    When I run "stems up --detach --json"
    Then the command succeeds
    And within 5s the JSON at "$.data.stems[0].history[2].rss_bytes" of "stems metrics --history 10s --json" is greater than 0
    When I run "stems metrics --history 10s --json"
    Then the command succeeds
    And the JSON at "$.data.stems[0].history" has between 1 and 40 elements
    And the JSON at "$.data.stems[0].history[*].ts" is in ascending order
    When I run "stems metrics echo-svc --json"
    Then the JSON at "$.data.stems[0].history" does not exist
    And the JSON at "$.data.stems[0].latest.uptime_s" exists
    And the JSON at "$.data.stems[0].latest.restarts" equals 0
    When I run "stems metrics nope --json"
    Then the JSON error has code "UNKNOWN_STEM"
    When I run "stems metrics --history soon --json"
    Then the exit code is 2
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
