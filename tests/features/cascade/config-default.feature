@FR-LC-9
Feature: restart.cascade makes a stem's restarts cascade by default
  With `restart: {cascade: true}` on a stem, a plain `stems restart` of it
  cascades to its running hard dependants; `--no-cascade` restarts only the
  stem (the flag always wins over the config).

  Scenario: a plain restart cascades; --no-cascade does not
    Given the fixture workspace "cascade-chain"
    And a local override setting stems.a.restart.cascade=true
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems restart a --json"
    Then the command succeeds
    And the JSON at "$.data.cascade.restarted" equals [["b", "c"], ["d"]]
    And there are exactly 1 events matching {"kind": "cascade.started", "data": {"origin": "a"}}
    And there are exactly 1 events matching {"kind": "stem.restarting", "stem": "d"}
    When I run "stems status --json"
    And I save the JSON at "$.data.stems[?@.name == 'd'].pid" as "d"
    When I run "stems restart a --no-cascade --json"
    Then the command succeeds
    And the JSON at "$.data.cascade" does not exist
    And there are exactly 1 events matching {"kind": "cascade.started"}
    And there are exactly 1 events matching {"kind": "stem.restarting", "stem": "d"}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name == 'd'].pid" equals ${var:d}
    When I run "stems down --json"
    Then the command succeeds
