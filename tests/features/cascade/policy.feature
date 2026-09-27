@FR-LC-6 @FR-LC-9
Feature: a policy restart with restart.cascade restarts the dependants
  With `restart: {policy: on-failure, cascade: true}` a crashed stem is
  restarted by its policy and, once it is healthy again, its running hard
  dependants are restarted (`cascade.started` with `reason: policy`, actor
  `daemon`). A second crash while that cascade runs does not start a
  nested cascade: the new one is queued (`cascade.queued`) and starts only
  after the first finished. `status` shows `cascade: {id, origin}` on the
  stems of the running cascade.

  Scenario: crash a twice; two cascades, one after the other
    Given the fixture workspace "cascade-chain"
    And the local override file is extended with:
      """
      stems:
        a:
          restart: { policy: on-failure, cascade: true }
        d:
          env: { SHOP_SLEEP_START: "3" }
      """
    When I run "stems up --detach --json"
    Then the command succeeds
    When the chaos endpoint "crash?code=3" is called on "a"
    Then within 10s the events stream contains {"kind": "cascade.started", "actor": "daemon", "data": {"origin": "a", "reason": "policy", "stems": [["b", "c"], ["d"]]}}
    And the events stream contains {"kind": "stem.restarting", "stem": "a", "data": {"counted": true}} before {"kind": "cascade.started"}
    And within 5s the JSON at "$.data.stems[?@.name == 'd'].cascade.origin" of "stems status --json" equals "a"
    # a is healthy again (the cascade waited for it): crash it during the cascade
    When the chaos endpoint "crash?code=3" is called on "a"
    Then within 10s the events stream contains {"kind": "cascade.queued", "actor": "daemon", "data": {"origin": "a", "reason": "policy"}}
    And within 30s there are at least 2 events matching {"kind": "cascade.finished", "data": {"origin": "a", "restarted": ["b", "c", "d"]}}
    When I run "stems events --json --since 0"
    And I save the JSON at "$[?@.kind == 'cascade.queued'].data.id" as "second"
    Then the events stream contains {"kind": "cascade.finished"} before {"kind": "cascade.started", "data": {"id": "${var:second}"}}
    And there are exactly 2 events matching {"kind": "cascade.started"}
    And there are exactly 2 events matching {"kind": "stem.restarting", "stem": "d"}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name == 'a'].restarts" equals 2
    And the JSON at "$.data.stems[?@.name == 'd'].restarts" equals 0
    And the JSON at "$.data.stems[?@.name == 'd'].state" equals "healthy"
    When I run "stems down --json"
    Then the command succeeds
