@FR-ST-3
Feature: External stems are monitored, never started or stopped
  An external stem is shown in `status` but `up` and `down` leave it alone:
  until health probes (21) its state is `unknown`, and a dependency edge to
  it (even `condition: healthy`) is satisfied immediately. `start`, `stop`
  and `restart` of an external stem fail with NOT_MANAGED (exit 1).

  @FR-HS-2
  Scenario: up reports the external stem unknown and starts its dependant
    Given the fixture workspace "with-external"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ok" equals true
    And the JSON at "$.data.ready" contains "api"
    And the JSON at "$.data.ready" contains "hosted"
    And the JSON at "$.data.failed" equals []
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='hosted'].state" equals "unknown"
    And the JSON at "$.data.stems[?@.name=='hosted'].glyph" equals "unknown"
    And the JSON at "$.data.stems[?@.name=='hosted'].type" equals "external"
    And the JSON at "$.data.stems[?@.name=='hosted'].pid" equals null
    And the JSON at "$.data.stems[?@.name=='api'].state" equals "healthy"
    And the JSON at "$.data.summary" equals {"healthy": 1, "degraded": 0, "failed": 0, "stopped": 0, "unknown": 1, "starting": 0}
    When I run "stems status hosted --human"
    Then the command succeeds
    And stdout contains "? unknown"
    When I run "stems down --all --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals ["api"]
    And the lock file and socket do not exist
    And no process from the workspace's process groups is alive

  @error
  Scenario: start, stop and restart of an external stem are NOT_MANAGED
    Given the fixture workspace "with-external"
    When I run "stems up api --detach --json"
    Then the command succeeds
    When I run "stems start hosted --json"
    Then the exit code is 1
    And the JSON error has code "NOT_MANAGED"
    And the error message contains "hosted"
    When I run "stems stop hosted --json"
    Then the exit code is 1
    And the JSON error has code "NOT_MANAGED"
    When I run "stems restart hosted --json"
    Then the exit code is 1
    And the JSON error has code "NOT_MANAGED"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='hosted'].state" equals "unknown"
    And the JSON at "$.data.stems[?@.name=='api'].state" equals "healthy"
    When I run "stems down --all --json"
    Then the command succeeds
