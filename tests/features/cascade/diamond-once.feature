@FR-LC-9
Feature: a cascade restarts every dependant at most once
  The dependants are the transitive closure over hard edges, ordered once:
  a diamond's join (d) restarts exactly once, after both of its branches,
  and a stem named in the same `restart` is an origin, never restarted
  again as a dependant (`stems restart a b --cascade` restarts a and b,
  then c, then d).

  Scenario: d restarts exactly once per cascade
    Given the fixture workspace "cascade-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems restart a --cascade --json"
    Then the command succeeds
    And within 5s the events stream contains {"kind": "cascade.finished", "data": {"origin": "a", "restarted": ["b", "c", "d"], "failed": []}}
    And there are exactly 1 events matching {"kind": "stem.restarting", "stem": "d"}
    And there are exactly 1 events matching {"kind": "stem.state", "stem": "d", "from": "healthy", "to": "starting"}
    When I run "stems down --json"
    Then the command succeeds

  Scenario: several origins: their dependants' union, ordered once
    Given the fixture workspace "cascade-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems restart a b --cascade --json"
    Then the command succeeds
    And the JSON at "$.data.cascade.origins" equals ["a", "b"]
    And the JSON at "$.data.cascade.restarted" equals [["c"], ["d"]]
    And there are exactly 1 events matching {"kind": "stem.restarting", "stem": "d"}
    And the events stream does not contain {"kind": "stem.restarting", "stem": "b"}
    And the events stream contains {"kind": "stem.restarting", "stem": "c"} before {"kind": "stem.restarting", "stem": "d"}
    When I run "stems down --json"
    Then the command succeeds
