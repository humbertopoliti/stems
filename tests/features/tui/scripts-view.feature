@FR-SC-2 @FR-SC-6 @FR-UI-1
Feature: The Scripts view lists and runs every script, the workspace's too
  `6` (the sixth tab) shows the whole `script_catalog` grouped by owner:
  the workspace-level scripts first, then each stem's. `Enter` runs the
  selected script (its form first when it declares `args`); the output
  streams into the split log pane and `script.finished` becomes a toast.
  The action bar counts the workspace's scripts next to the stem's.

  Background:
    Given the fixture workspace "run-scripts"
    When I run "stems up --detach --json"
    Then the command succeeds

  Scenario: the view lists the workspace script, then shop-api's
    When I run "stems attach --view table --headless --size 120x40 --script 'wait:healthy;frame;6;frame'"
    Then the command succeeds
    And frame 1 contains ": scripts (8+1)"
    And frame 2 contains "6 [Scripts]"
    And frame 2 contains "workspace · global"
    And frame 2 contains "› needs-api"
    And frame 2 contains "A workspace script that needs shop-api healthy"
    And frame 2 contains "shop-api · ✓ healthy"
    And frame 2 contains "create-test-user custom    (email*, role) Create a user with a known password"

  Scenario: Enter runs the workspace script, attributed to the TUI
    When I run "stems attach --headless --size 120x40 --script 'wait:healthy;6;Enter;wait:event=script.finished;wait:log=needs-api: shop-api is at;frame'"
    Then the command succeeds
    And the last frame contains "✓ needs-api finished in"
    And the last frame contains "needs-api: shop-api is at"
    And within 5s the events stream contains {"kind": "script.finished", "data": {"script": "needs-api", "ok": true}}
    When I run "stems events --json --since 0"
    Then the JSON at "$[?@.kind=='script.started'].actor" contains "tui:"

  Scenario: / filters by script name; Enter opens the form of a script with args
    When I run "stems attach --headless --size 120x40 --script 'wait:healthy;6;/create<Enter>;frame;Enter;frame'"
    Then the command succeeds
    And frame 1 contains "Scripts · /create"
    And frame 1 contains "› create-test-user"
    And frame 1 does not contain "needs-api"
    And frame 2 contains "Run create-test-user (shop-api)"
