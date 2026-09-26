@FR-SC-2 @FR-UI-2
Feature: The script menu runs a stem's scripts with arguments
  `:` lists the selected stem's scripts (lifecycle and custom, from
  `script_catalog`) with their descriptions, then the workspace's; typing
  filters. A script with `args` opens a form (defaults prefilled);
  `Enter` runs it with `run_script {wait: false}`. Its tagged output
  streams into the split log pane and `script.finished` becomes a toast.

  Background:
    Given the fixture workspace "run-scripts"
    When I run "stems up --detach --json"
    Then the command succeeds

  Scenario: the menu lists create-test-user with its description
    When I run "stems attach --headless --script 'wait:healthy;:;frame'"
    Then the command succeeds
    And the frame matches golden "tui-script-menu" masking PID,UPTIME,CPU,MEM
    And the last frame contains "create-test-user …  Create a user with a known password"

  Scenario: the form runs create-test-user; output and toast appear
    When I run "stems attach --headless --size 120x40 --script 'wait:healthy;:;type:create-test-user;Enter;type:a@b.c;frame;Enter;wait:event=script.finished:shop-api;wait:log=create-test-user: ok;frame'"
    Then the command succeeds
    And frame 1 contains "Run create-test-user (shop-api)"
    And frame 1 contains "email*  a@b.c_"
    And frame 1 contains "‹ admin ›"
    And the last frame contains "✓ create-test-user finished in"
    And the last frame contains "[create-test-user] create-test-user: ok (email=a@b.c"
    And within 5s the events stream contains {"kind": "script.finished", "stem": "shop-api", "data": {"script": "create-test-user", "ok": true}}
    When I run "stems events --json --since 0"
    Then the JSON at "$[?@.kind=='script.started'].actor" contains "tui:"
