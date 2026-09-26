@FR-WD-3 @error
Feature: an invalid config change keeps the applied config
  A change that does not validate emits `config.invalid {errors, codes}`;
  the applied config stays in force (`status` is unaffected) and `config
  diff` reports `pending: false` with the errors. Fixing the file emits
  `config.changed` again. Fixture: tests/fixtures/workspaces/process-chain.

  Scenario: a dependency cycle, then its fix
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    Given a local override setting stems.a.depends_on=[d]
    Then within 5s the events stream contains {"kind": "config.invalid", "data": {"codes": ["CYCLE"]}}
    When I run "stems status --json"
    Then the command succeeds
    And the JSON at "$.data.stems[?@.name=='a'].state" equals "healthy"
    And the JSON at "$.data.summary.healthy" equals 4
    When I run "stems config diff --json"
    Then the command succeeds
    And the JSON at "$.data.pending" equals false
    And the JSON at "$.data.last_error" contains {"code": "CYCLE"}
    Given a local override setting stems.a.depends_on=[]
    Then within 5s the events stream contains {"kind": "config.changed"}
    When I run "stems config diff --json"
    Then the JSON at "$.data.pending" equals false
    And the JSON at "$.data.last_error" does not exist
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
