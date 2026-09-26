@FR-WD-3
Feature: config apply restarts only the stems that need it
  `stems config apply --yes` stops the affected stems in reverse dependency
  order, installs the new config and starts them again in dependency order.
  Stems whose config did not change keep running, dependants included (an
  edge condition only gates a start). Fixture:
  tests/fixtures/workspaces/process-chain (a ← b, c; b, c ← d).

  Scenario: only b restarts, with its new env
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status --json"
    And I save the JSON at "$.data.stems[?@.name=='a'].pid" as "a_pid"
    And I save the JSON at "$.data.stems[?@.name=='b'].pid" as "b_pid"
    And I save the JSON at "$.data.stems[?@.name=='c'].pid" as "c_pid"
    And I save the JSON at "$.data.stems[?@.name=='d'].pid" as "d_pid"
    Given a local override setting stems.b.env.RELOAD_MARK=applied
    Then within 5s the events stream contains {"kind": "config.changed", "data": {"plan": {"stems": [{"name": "b", "action": "restart_required"}]}}}
    When I run "stems config apply --json"
    Then the exit code is 2
    And the JSON error has code "DESTRUCTIVE_NOT_CONFIRMED"
    When I run "stems config apply --yes --json"
    Then the command succeeds
    And the JSON at "$.data.applied[?@.stem=='b'].result" equals "restarted"
    And the JSON at "$.data.failed" equals []
    And the JSON at "$.data.pending" equals false
    And the events stream contains {"kind": "stem.state", "stem": "b", "to": "stopped", "reason": "config reload"} before {"kind": "stem.state", "stem": "b", "to": "starting", "reason": "config reload"}
    And within 5s the events stream contains {"kind": "config.applied", "data": {"applied": [{"stem": "b", "action": "restart_required", "result": "restarted"}]}}
    When I run "stems status --verbose --json"
    Then the JSON at "$.data.stems[?@.name=='b'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='b'].pid" does not equal ${var:b_pid}
    And the JSON at "$.data.stems[?@.name=='b'].env.RELOAD_MARK" equals "applied"
    And the JSON at "$.data.stems[?@.name=='a'].pid" equals ${var:a_pid}
    And the JSON at "$.data.stems[?@.name=='c'].pid" equals ${var:c_pid}
    And the JSON at "$.data.stems[?@.name=='d'].pid" equals ${var:d_pid}
    When I run "stems config diff --json"
    Then the JSON at "$.data.pending" equals false
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
