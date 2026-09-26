@FR-MT-1
Feature: process stems are sampled from their process group
  Every `metrics.interval` the daemon reads the stem's process group:
  `rss_bytes` is the resident memory summed over the tree, `children` the
  number of processes in it (the leader included). Forked children stay in
  the group and are counted; memory the stem allocates shows up within a few
  samples. The fixture samples every 300 ms; bounds are loose for a loaded
  machine.

  Scenario: rss and children follow fork and alloc
    Given the "minimal" workspace with a local override setting metrics.interval=300ms
    When I run "stems up --detach --json"
    Then the command succeeds
    And within 5s the JSON at "$.data.stems[0].latest.rss_bytes" of "stems metrics --json" is greater than 1048576
    And the JSON at "$.data.stems[0].latest.children" equals 1
    And the JSON at "$.data.stems[0].type" equals "process"
    And the JSON at "$.data.interval_ms" equals 300
    When the chaos endpoint "fork?n=2" is called on "echo-svc"
    Then within 5s the JSON at "$.data.stems[0].latest.children" of "stems metrics --json" equals 3
    And the JSON at "$.data.totals.children" equals 3
    When I save the JSON at "$.data.stems[0].latest.rss_bytes" as "rss"
    And the chaos endpoint "alloc?mb=150" is called on "echo-svc"
    Then the chaos response status is 200
    And within 5s the JSON at "$.data.stems[0].latest.rss_bytes" of "stems metrics --json" is greater than ${var:rss}+104857600
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].metrics.children" equals 3
    And the JSON at "$.data.stems[0].metrics.rss_bytes" is greater than 104857600
    When I run "stems stop echo-svc --json"
    Then the command succeeds
    When I run "stems metrics --json"
    Then the JSON at "$.data.stems[0].latest" equals null
    And the JSON at "$.data.totals.children" equals 0
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
