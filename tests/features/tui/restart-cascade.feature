@FR-LC-9 @FR-UI-2
Feature: r asks whether to restart the stem's running dependants too
  `r` on a stem with running hard dependants (from the dependency graph,
  loaded on demand) asks "also restart N dependants (b, c, d)? [y/N]":
  `y` sends `restart {cascade: true}`, `n` restarts only the stem
  (`cascade: false`), `Esc` cancels. A leaf restarts at once.
  process-chain is a diamond: b and c depend on a, d on b and c.

  Background:
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds

  Scenario: r on a, then y, restarts a and its dependants
    When I run "stems attach --view table --headless --script 'wait:healthy;r;frame;y;wait:healthy;frame'"
    Then the command succeeds
    And frame 1 contains "also restart 3 dependants (b, c, d)? [y/N]"
    And the last frame contains "✓ restarted a"
    And within 10s the events stream contains {"kind": "cascade.started"}
    When I run "stems events --json --since 0"
    Then the JSON at "$[?@.kind=='cascade.started'].actor" contains "tui:"

  Scenario: r on a, then n, restarts only a
    When I run "stems status --json"
    And I save the JSON at "$.data.stems[?@.name=='d'].pid" as "d_pid"
    When I run "stems attach --view table --headless --script 'wait:healthy;r;n;wait:healthy;frame'"
    Then the command succeeds
    And the last frame contains "✓ restarted a"
    And the events stream does not contain {"kind": "cascade.started"}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='d'].pid" equals ${var:d_pid}
