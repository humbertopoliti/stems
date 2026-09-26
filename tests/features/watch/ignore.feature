@FR-WD-1
Feature: ignored paths never trigger a watch rule
  Built-in ignores (`.git`, `node_modules`, `target`, `dist`, `build`,
  `.venv`, `__pycache__`, `*.log`, `.stems`) always apply, merged with a
  rule's own `ignore:`, and ignore wins over `paths` — even `paths: ["**"]`.

  Scenario: node_modules/x.js is ignored, src/x.js is not
    Given the fixture workspace "watch-shop"
    And the workspace has a private copy of the repos
    When I run "stems up anyfile --detach --json"
    Then the command succeeds
    And within 15s the stem "anyfile" is "healthy"
    Given the file "${tmp}/examples/repos/shop-api/node_modules/x.js" is written with "module.exports = 1"
    And the file "${tmp}/examples/repos/shop-api/server.log" is written with "a log line"
    Then during 2s the events stream never contains {"kind": "watch.triggered", "stem": "anyfile"}
    Given the file "${tmp}/examples/repos/shop-api/src/x.js" is written with "module.exports = 2"
    Then within 5s the events stream contains {"kind": "watch.triggered", "stem": "anyfile", "data": {"paths": ["src/x.js"]}}
    And within 20s the events stream contains {"kind": "watch.action_finished", "stem": "anyfile", "data": {"ok": true}}
    When I run "stems down --json"
    Then the command succeeds
