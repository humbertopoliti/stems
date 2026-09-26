@FR-WD-3
Feature: stems added to or removed from the config
  A stem added in stems.local.yaml is `added`; `config apply` starts it
  when the last `up` covers it (here: every stem). A removed stem is
  `removed`; `config apply` stops it, cleans its overlays and drops its
  state entry, and it leaves `status`. Fixture:
  tests/fixtures/workspaces/process-chain.

  Scenario: add a stem, apply, remove it, apply
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    Given the local override file is extended with:
      """
      stems:
        extra:
          type: process
          codebase: ../../../../examples/repos/shop-api
          depends_on: [a]
          env: { SHOP_SLEEP_START: "0" }
          ports: [{ name: http, port: auto }]
          health: { type: tcp, interval: 200ms, timeout: 150ms, retries: 5, start_timeout: 10s }
          stop_grace: 1s
          scripts: { start: python3 app.py }
      """
    Then within 5s the events stream contains {"kind": "config.changed", "data": {"plan": {"stems": [{"name": "extra", "action": "added"}]}}}
    When I run "stems config apply --yes --json"
    Then the command succeeds
    And the JSON at "$.data.applied[?@.stem=='extra'].result" equals "started"
    And within 10s the stem "extra" is "healthy"
    Given the local override "stems.extra" is removed
    Then within 5s the events stream contains {"kind": "config.changed", "data": {"plan": {"stems": [{"name": "extra", "action": "removed", "running": true}]}}}
    When I run "stems config apply --yes --json"
    Then the command succeeds
    And the JSON at "$.data.applied[?@.stem=='extra'].result" equals "stopped"
    And within 5s the events stream contains {"kind": "stem.state", "stem": "extra", "to": "stopped", "reason": "config reload"}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='extra']" does not exist
    And the JSON at "$.data.summary.healthy" equals 4
    When I run the shell command "cat ${home}/*/state.json"
    Then stdout contains "pgid"
    And stdout does not contain "extra"
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
