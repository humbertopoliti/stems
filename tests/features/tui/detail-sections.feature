@FR-UI-2 @FR-SC-2
Feature: The Detail view lists the stem's scripts and runs them
  Detail shows the selected stem in sections: a header, Scripts (one
  selectable row per script: name, kind, last run, args, description),
  Variants and Watchdog (when the stem has them), then Health, Recent
  events and Config. `j`/`k` move over the rows of every section; `Enter`
  on a script runs it, opening its argument form first when it declares
  `args`. run-scripts' shop-api lists its custom scripts by name:
  always-fails, create-test-user, echo-args, ...

  Background:
    Given the fixture workspace "run-scripts"
    When I run "stems up --detach --json"
    Then the command succeeds

  Scenario: the Scripts section lists create-test-user
    When I run "stems attach --headless --size 120x40 --script 'wait:healthy;3;frame'"
    Then the command succeeds
    And the last frame contains "Detail: shop-api"
    And the last frame contains "Scripts · Enter run · : menu"
    And the last frame contains "› always-fails     custom"
    And the last frame contains "  create-test-user custom    (email*, role) Create a user with a known password"
    And the last frame contains ": scripts (8)"

  Scenario: j then Enter runs create-test-user through its form
    When I run "stems attach --headless --size 120x40 --script 'wait:healthy;3;j;Enter;type:a@b.c;frame;Enter;wait:event=script.finished:shop-api;frame'"
    Then the command succeeds
    And frame 1 contains "Run create-test-user (shop-api)"
    And frame 1 contains "email*  a@b.c_"
    And frame 2 contains "✓ create-test-user finished in"
    And frame 2 contains "› create-test-user custom    ✓"
    And within 5s the events stream contains {"kind": "script.finished", "stem": "shop-api", "data": {"script": "create-test-user", "ok": true}}
    When I run "stems events --json --since 0"
    Then the JSON at "$[?@.kind=='script.started' && @.data.script=='create-test-user'].actor" contains "tui:"
