@FR-WD-2
Feature: watchdogs can be paused and resumed, per stem or globally
  `stems watch pause [stem…]` stops rules from firing (changes made while
  paused are dropped, not queued) and emits `watch.paused`; `stems watch
  resume` brings them back (`watch.resumed`). Without stems the pause is
  global; a global resume also clears per-stem pauses. `stems watch status
  --json` shows each stem's `paused` flag and `global_paused`.

  Scenario: pause one stem, change a file, resume, change again
    Given the fixture workspace "watch-shop"
    And the workspace has a private copy of the repos
    When I run "stems up api --detach --json"
    Then the command succeeds
    And within 15s the stem "api" is "healthy"
    When I run "stems watch pause api --json"
    Then the command succeeds
    And within 2s the events stream contains {"kind": "watch.paused", "stem": "api"}
    When I run "stems watch status --json"
    Then the command succeeds
    And the JSON at "$.data.stems[?@.name == 'api'].paused" equals true
    And the JSON at "$.data.stems[?@.name == 'api'].active" equals true
    And the JSON at "$.data.global_paused" equals false
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].watch.paused" equals true
    When I run the shell command "echo '# while paused' >> ${tmp}/examples/repos/shop-api/app.py"
    Then during 2s the events stream never contains {"kind": "watch.triggered", "stem": "api"}
    When I run "stems watch resume api --json"
    Then the command succeeds
    And within 2s the events stream contains {"kind": "watch.resumed", "stem": "api"}
    When I run "stems watch status --json"
    Then the JSON at "$.data.stems[?@.name == 'api'].paused" equals false
    When I run the shell command "echo '# resumed' >> ${tmp}/examples/repos/shop-api/app.py"
    Then within 5s the events stream contains {"kind": "watch.triggered", "stem": "api", "data": {"paths": ["app.py"]}}
    And within 20s the events stream contains {"kind": "watch.action_finished", "stem": "api", "data": {"ok": true}}
    When I run "stems down --json"
    Then the command succeeds

  Scenario: a global pause silences every watchdog
    Given the fixture workspace "watch-shop"
    And the workspace has a private copy of the repos
    When I run "stems up api --detach --json"
    Then the command succeeds
    And within 15s the stem "api" is "healthy"
    When I run "stems watch pause --json"
    Then the command succeeds
    And the JSON at "$.data.global_paused" equals true
    When I run "stems watch status --json"
    Then the JSON at "$.data.global_paused" equals true
    And the JSON at "$.data.stems[?@.name == 'api'].paused" equals true
    When I run the shell command "echo '# while paused' >> ${tmp}/examples/repos/shop-api/app.py"
    Then during 2s the events stream never contains {"kind": "watch.triggered"}
    When I run "stems watch resume --json"
    Then the command succeeds
    And the JSON at "$.data.global_paused" equals false
    When I run the shell command "echo '# resumed' >> ${tmp}/examples/repos/shop-api/app.py"
    Then within 5s the events stream contains {"kind": "watch.triggered", "stem": "api"}
    And within 20s the events stream contains {"kind": "watch.action_finished", "stem": "api", "data": {"ok": true}}
    When I run "stems down --json"
    Then the command succeeds
