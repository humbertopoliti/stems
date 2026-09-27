@FR-LC-9 @error
Feature: a cascade whose origin does not come back touches no dependant
  The origin must be healthy again (bounded by its `health.start_timeout`)
  before any dependant restarts. When it fails the cascade stops:
  `cascade.aborted {stem, error}`, the dependants keep running untouched
  (same pids), and `restart` fails with the origin's error.

  Scenario: a fails to start again; b, c and d keep their pids
    Given the fixture workspace "cascade-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status --json"
    And I save the JSON at "$.data.stems[?@.name == 'b'].pid" as "b"
    And I save the JSON at "$.data.stems[?@.name == 'c'].pid" as "c"
    And I save the JSON at "$.data.stems[?@.name == 'd'].pid" as "d"
    Given the file "fail-a" is written with "make a's start exit 3"
    When I run "stems restart a --cascade --json"
    Then the command fails
    And the JSON at "$.data.cascade.aborted" equals true
    And the JSON at "$.data.cascade.restarted" equals []
    And the JSON at "$.data.failed[0].stem" equals "a"
    And within 5s the events stream contains {"kind": "cascade.aborted", "stem": "a", "data": {"origin": "a"}}
    And the events stream does not contain {"kind": "cascade.finished"}
    And the events stream does not contain {"kind": "stem.restarting"}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name == 'a'].state" equals "failed"
    And the JSON at "$.data.stems[?@.name == 'b'].pid" equals ${var:b}
    And the JSON at "$.data.stems[?@.name == 'c'].pid" equals ${var:c}
    And the JSON at "$.data.stems[?@.name == 'd'].pid" equals ${var:d}
    And the JSON at "$.data.stems[?@.name == 'd'].state" equals "healthy"
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
