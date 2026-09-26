@FR-MT-1
Feature: CPU % is the process tree's CPU time over wall time
  `cpu_pct` is the growth of the tree's CPU time between two samples over
  the wall time between them, in percent of one core. A stem busy-looping
  one thread for 3 s shows a sample above 50 % (one busy core is ~100 %;
  the bound is loose so a loaded machine does not make it flaky).

  Scenario: a spinning stem shows high CPU in its history
    Given the "minimal" workspace with a local override setting metrics.interval=300ms
    When I run "stems up --detach --json"
    Then the command succeeds
    And within 5s the JSON at "$.data.stems[0].latest.children" of "stems metrics --json" equals 1
    When the chaos endpoint "spin?ms=3000" is called on "echo-svc"
    Then the chaos response status is 200
    And within 5s some sample in the JSON at "$.data.stems[0].history" of "stems metrics --history 10s --json" has "cpu_pct" greater than 50
    And some sample in the JSON at "$.data.stems[0].history" has "cpu_pct" greater than 50
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
