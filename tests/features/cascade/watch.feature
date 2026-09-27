@FR-WD-1 @FR-LC-9
Feature: a watch rule with cascade restarts the stem's dependants
  A watch rule with `cascade: true` restarts its stem and then, once it is
  healthy, its running hard dependants. Triggers of stems the cascade is
  restarting are dropped, not queued (loop guard #4), and a restart caused
  by a cascade never starts one (guard #1): here b, c and d watch the same
  file with `cascade: true` too (a longer debounce, so they fire while the
  cascade runs), yet there is exactly one cascade and d restarts once.

  Scenario: touching a's file cascades once through the diamond
    Given the fixture workspace "cascade-chain"
    And the workspace has a private copy of the repos
    And the local override file is extended with:
      """
      stems:
        a:
          watch: [{ paths: ["cascade_trigger.py"], debounce: 200ms, cascade: true }]
        b:
          watch: [{ paths: ["cascade_trigger.py"], debounce: 1500ms, cascade: true }]
        c:
          watch: [{ paths: ["cascade_trigger.py"], debounce: 1500ms, cascade: true }]
        d:
          env: { SHOP_SLEEP_START: "2" }
          watch: [{ paths: ["cascade_trigger.py"], debounce: 1500ms, cascade: true }]
      """
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status --json"
    And I save the JSON at "$.data.stems[?@.name == 'd'].pid" as "d"
    When I run the shell command "echo '# touched' >> ${tmp}/examples/repos/shop-api/cascade_trigger.py"
    Then the exit code is 0
    And within 10s the events stream contains {"kind": "watch.triggered", "stem": "a", "data": {"paths": ["cascade_trigger.py"]}}
    And within 15s the events stream contains {"kind": "cascade.finished", "actor": "watchdog", "data": {"origin": "a", "restarted": ["b", "c", "d"]}}
    And within 10s the events stream contains {"kind": "watch.action_finished", "stem": "a", "data": {"ok": true}}
    And during 5s there are at most 1 events matching {"kind": "cascade.started"}
    And there are exactly 1 events matching {"kind": "cascade.started", "actor": "watchdog", "data": {"origin": "a"}}
    And there are exactly 1 events matching {"kind": "stem.restarting", "stem": "d"}
    And there are exactly 1 events matching {"kind": "watch.triggered"}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name == 'd'].pid" does not equal ${var:d}
    And the JSON at "$.data.stems[?@.name == 'd'].state" equals "healthy"
    When I run "stems down --json"
    Then the command succeeds
