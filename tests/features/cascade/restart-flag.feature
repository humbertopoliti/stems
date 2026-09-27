@FR-LC-3 @FR-LC-9
Feature: stems restart --cascade also restarts the stem's dependants
  `stems restart <stem> --cascade` restarts the stem, waits until it is
  healthy again, then restarts its running hard dependants layer by layer
  (`start_order` restricted to them): in the `cascade-chain` diamond (b and
  c depend on a, d on b and c) b and c restart together once a is back,
  and d exactly once, after both. Each dependant's restart is a policy
  bypass (`stem.restarting` with `cascade: <id>`, `counted: false`). Without
  the flag only the named stem restarts.

  Scenario: restart a --cascade restarts b and c, then d once; every pid changes
    Given the fixture workspace "cascade-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status --json"
    And I save the JSON at "$.data.stems[?@.name == 'a'].pid" as "a"
    And I save the JSON at "$.data.stems[?@.name == 'b'].pid" as "b"
    And I save the JSON at "$.data.stems[?@.name == 'c'].pid" as "c"
    And I save the JSON at "$.data.stems[?@.name == 'd'].pid" as "d"
    When I run "stems restart a --cascade --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["a"]
    And the JSON at "$.data.cascade.origin" equals "a"
    And the JSON at "$.data.cascade.restarted" equals [["b", "c"], ["d"]]
    And the JSON at "$.data.cascade.failed" equals []
    And the events stream contains {"kind": "cascade.started", "stem": "a", "data": {"origin": "a", "reason": "restart", "stems": [["b", "c"], ["d"]]}} before {"kind": "stem.state", "stem": "a", "to": "stopping"}
    # a first, then b and c (either order), then d
    And the events stream contains {"kind": "stem.state", "stem": "a", "to": "stopping"} before {"kind": "stem.restarting", "stem": "b"}
    And the events stream contains {"kind": "stem.state", "stem": "a", "to": "stopping"} before {"kind": "stem.restarting", "stem": "c"}
    And the events stream contains {"kind": "stem.restarting", "stem": "b"} before {"kind": "stem.restarting", "stem": "d"}
    And the events stream contains {"kind": "stem.restarting", "stem": "c"} before {"kind": "stem.restarting", "stem": "d"}
    And the events stream contains {"kind": "stem.restarting", "stem": "d"} before {"kind": "cascade.finished", "data": {"restarted": ["b", "c", "d"]}}
    And within 1s the events stream contains {"kind": "stem.restarting", "stem": "d", "data": {"counted": false, "reason": "cascade from a"}}
    And there are exactly 1 events matching {"kind": "stem.restarting", "stem": "d"}
    And there are exactly 1 events matching {"kind": "stem.restarting", "stem": "b"}
    And there are exactly 1 events matching {"kind": "stem.restarting", "stem": "c"}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name == 'a'].pid" does not equal ${var:a}
    And the JSON at "$.data.stems[?@.name == 'b'].pid" does not equal ${var:b}
    And the JSON at "$.data.stems[?@.name == 'c'].pid" does not equal ${var:c}
    And the JSON at "$.data.stems[?@.name == 'd'].pid" does not equal ${var:d}
    And the JSON at "$.data.stems[?@.name == 'd'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name == 'd'].restarts" equals 0
    And the JSON at "$.data.stems[?@.name == 'd'].cascade" does not exist
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: restart a without the flag restarts only a
    Given the fixture workspace "cascade-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status --json"
    And I save the JSON at "$.data.stems[?@.name == 'a'].pid" as "a"
    And I save the JSON at "$.data.stems[?@.name == 'd'].pid" as "d"
    When I run "stems restart a --json"
    Then the command succeeds
    And the JSON at "$.data.cascade" does not exist
    And the events stream does not contain {"kind": "cascade.started"}
    And the events stream does not contain {"kind": "stem.restarting"}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name == 'a'].pid" does not equal ${var:a}
    And the JSON at "$.data.stems[?@.name == 'd'].pid" equals ${var:d}
    When I run "stems down --json"
    Then the command succeeds
