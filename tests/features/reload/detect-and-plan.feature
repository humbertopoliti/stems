@FR-WD-3
Feature: a config change is detected and planned, not applied
  The daemon watches stems.yaml, stems.local.yaml and included files. A
  change is loaded and validated after 300 ms of quiet and compared with
  the applied config: `config.changed {plan}` lists what applying it would
  do per stem. Nothing is applied until `stems config apply` (unless
  `config.reload.auto_apply`). Fixture: tests/fixtures/workspaces/process-chain
  (a ← b, c; b, c ← d).

  Scenario: an env var added in stems.local.yaml plans a restart of that stem only
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status --json"
    And I save the JSON at "$.data.stems[?@.name=='b'].pid" as "b_pid"
    Given a local override setting stems.b.env.RELOAD_MARK=1
    Then within 5s the events stream contains {"kind": "config.changed", "data": {"plan": {"stems": [{"name": "b", "action": "restart_required", "fields": ["env"], "hot": false}]}}}
    When I run "stems config diff --json"
    Then the command succeeds
    And the JSON at "$.data.pending" equals true
    And the JSON at "$.data.plan.stems[?@.name=='b'].action" equals "restart_required"
    And the JSON at "$.data.plan.stems[?@.name=='b'].fields" contains "env"
    And the JSON at "$.data.plan.stems[?@.name=='b'].running" equals true
    And the JSON at "$.data.plan.stems[?@.name=='a'].action" equals "unchanged"
    And the JSON at "$.data.plan.stems[?@.name=='d'].action" equals "unchanged"
    And the JSON at "$.data.loaded_at" exists
    # Nothing restarted.
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='b'].pid" equals ${var:b_pid}
    And the events stream does not contain {"kind": "stem.state", "stem": "b", "to": "stopping"}
    When I run "stems status --verbose --json"
    Then the JSON at "$.data.stems[?@.name=='b'].env.RELOAD_MARK" does not exist
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
