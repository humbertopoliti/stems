@slow @NFR-1
Feature: probe overhead stays small
  Twenty process stems with a tcp probe every 200 ms (ten times the default
  rate) keep the daemon well below one CPU: measured ~3 % in a debug build
  on an idle machine; the bound is 10 % so a loaded machine (other e2e runs,
  builds) does not make it flaky. Waits are bounded by the stems'
  `start_timeout` (60 s) and the scenario budget; `down` of a group that
  already exited (zombies, EPERM on macOS) counts as stopped.

  Scenario: 20 stems probed every 200 ms
    Given a workspace with 20 process stems using tcp health every 200ms
    When I run "stems up --detach --max-parallel 8 --json"
    Then the command succeeds
    And the daemon's CPU is below 10 %
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
