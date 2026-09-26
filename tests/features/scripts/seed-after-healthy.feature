@FR-SC-1 @FR-GR-1 @FR-LC-1
Feature: seed runs after healthy and gates condition: seeded
  Once a stem is healthy, its `seed` script runs (state `seeding`) when the
  seed stamp is missing or changed; the stem is then `healthy` with
  `seeded: true`. A dependant with `condition: seeded` starts only after the
  seed finished.

  Scenario: the seeded dependant waits for the seed
    Given the fixture workspace "scripts-seed"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["db", "api"]
    And the events stream contains {"kind": "stem.state", "stem": "db", "to": "healthy"} before {"kind": "script.started", "stem": "db", "data": {"script": "seed"}}
    And the events stream contains {"kind": "stem.state", "stem": "db", "to": "seeding"} before {"kind": "script.started", "stem": "db", "data": {"script": "seed"}}
    And the events stream contains {"kind": "script.finished", "stem": "db", "data": {"script": "seed", "exit": 0}} before {"kind": "stem.state", "stem": "api", "to": "starting"}
    And within 5s the events stream contains {"kind": "stem.state", "stem": "db", "from": "seeding", "to": "healthy", "reason": "seeded"}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='db'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='db'].seeded" equals true
    And the JSON at "$.data.stems[?@.name=='api'].seeded" equals false
    When I run "stems logs db --script seed --json"
    Then the JSON nodes at "$[*].text" equal ["seeding", "seeded on port ${port:18511}"]
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: a current seed stamp skips the seed but still counts as seeded
    Given the fixture workspace "scripts-seed"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems down --json"
    Then the command succeeds
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["db", "api"]
    And the events stream does not contain {"kind": "script.started", "data": {"script": "seed"}}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='db'].seeded" equals true
    When I run "stems down --all --json"
    Then the command succeeds
