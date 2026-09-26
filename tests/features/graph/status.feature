@FR-GR-4
Feature: stems graph shows live status glyphs
  When the daemon runs, each box carries the stem's live glyph (`--status`
  is the default then); `--watch <interval>` redraws until Ctrl-C.

  Scenario: a running stem is ✓ and --no-status is config only
    Given the "minimal" workspace is up
    When I run "stems graph --json"
    Then the command succeeds
    And the JSON at "$.data.nodes[0].name" equals "echo-svc"
    And the JSON at "$.data.nodes[0].glyph" equals "✓"
    And the JSON at "$.data.nodes[0].status" equals "healthy"
    When I run "stems graph --no-color"
    Then the command succeeds
    And stdout contains "echo-svc ✓"
    When I run "stems graph --no-status --json"
    Then the command succeeds
    And the JSON at "$.data.nodes[0].glyph" equals "·"
    When I run "stems down --all --json"
    Then the command succeeds

  @error
  Scenario: --status without a daemon is DAEMON_NOT_RUNNING
    Given the "minimal" workspace
    When I run "stems graph --status --json"
    Then the exit code is 4
    And the JSON error has code "DAEMON_NOT_RUNNING"

  Scenario: a crash shows ✗ in the --watch output
    Given the "minimal" workspace with a local override setting stems.echo-svc.restart.policy=never
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems graph --watch 0.2 --human" in the background
    Then within 10s the background command's stdout contains "echo-svc ✓"
    When the chaos endpoint "crash" is called on "echo-svc"
    Then within 3s the background command's stdout contains "exited with code 1"
    And within 1s the background command's stdout contains "✗ │"
    When the background command is stopped
    Then within 5s the background command exits with code 0
    When I run "stems down --all --json"
    Then the command succeeds
