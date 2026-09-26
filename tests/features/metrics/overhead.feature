@slow @FR-MT-1
Feature: sampling twenty stems is cheap
  The sampler reads every process tree in one blocking call per interval;
  with 20 stems at the default 2 s interval (probes slowed to 2 s too) the
  daemon stays under 3 % of one CPU.

  Scenario: 20 stems sampled every 2 s
    Given a workspace with 20 process stems using tcp health every 2000ms
    When I run "stems up --detach --max-parallel 8 --json"
    Then the command succeeds
    And within 10s the JSON at "$.data.totals.children" of "stems metrics --json" is greater than 19
    And the daemon's CPU is below 3 %
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
