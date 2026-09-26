@FR-ST-6 @FR-WS-10
Feature: outputs are published to dependants
  A stem's `outputs:` are evaluated once its health check passes and before
  it counts as `healthy`, so a dependant with `condition: healthy` starts
  with them: `${stem.<name>.outputs.X}` in its env and
  `STEMS_<NAME>_OUTPUT_<X>` for every output of a direct dependency. A
  `secret: true` command output is shown as `<redacted>` everywhere
  (`status --verbose`, `outputs`), its command runs quietly (no log lines,
  no `script.*` events), and `show` only has its declaration.
  Fixture: tests/fixtures/workspaces/outputs-demo (api <- web).

  Scenario: a static and a secret command output reach the dependant
    Given the fixture workspace "outputs-demo"
    When I run "stems up --detach --json web"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api", "web"]
    And the events stream contains {"kind": "stem.outputs", "stem": "api", "data": {"names": ["API_URL", "TOKEN"], "secret": ["TOKEN"]}} before {"stem": "api", "to": "healthy"}
    And the events stream contains {"kind": "stem.outputs", "stem": "api"} before {"stem": "web", "to": "starting"}
    And the events stream does not contain {"kind": "script.started", "data": {"output": "TOKEN"}}
    Then within 5s the stem log file "web/current.log" contains "api_url=http://localhost:${port:18481}"
    When I run "stems logs web --json"
    Then the JSON at "$..text" contains "api_url=http://localhost:${port:18481}"
    When I run "stems logs --json"
    Then stdout does not contain "tok-${port:18481}"
    When I run "stems status --verbose --json web"
    Then the command succeeds
    And the JSON at "$.data.stems[0].env.API_URL" equals "http://localhost:${port:18481}"
    And the JSON at "$.data.stems[0].env.STEMS_API_OUTPUT_API_URL" equals "http://localhost:${port:18481}"
    And the JSON at "$.data.stems[0].env.STEMS_API_OUTPUT_TOKEN" equals "<redacted>"
    And stdout does not contain "tok-${port:18481}"
    When I run "stems status --json api"
    Then the JSON at "$.data.stems[0].outputs" equals {"API_URL": "http://localhost:${port:18481}", "TOKEN": "<redacted>"}
    When I run "stems outputs api --json"
    Then the command succeeds
    And the JSON at "$.data.stems[0].name" equals "api"
    And the JSON at "$.data.stems[0].outputs" equals [{"name": "API_URL", "value": "http://localhost:${port:18481}", "secret": false}, {"name": "TOKEN", "value": "<redacted>", "secret": true}]
    When I run "stems outputs api --json --reveal"
    Then the JSON at "$.data.stems[0].outputs[1].value" equals "<redacted>"
    When I run "stems show api --json"
    Then the command succeeds
    And the JSON at "$.data.outputs.TOKEN" equals {"command": "echo tok-$PORT", "secret": true}
    And the JSON at "$.data.outputs.API_URL" equals "http://localhost:${port:18481}"
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: outputs are listed but not evaluated while the stem is stopped
    Given the fixture workspace "outputs-demo"
    When I run "stems daemon start --json"
    Then the command succeeds
    When I run "stems outputs --json"
    Then the command succeeds
    And the JSON nodes at "$.data.stems[*].name" equal ["api", "counter", "broken-output"]
    And the JSON at "$.data.stems[0].outputs[0].value" equals null
    When I run "stems daemon stop --json"
    Then the command succeeds
