@FR-UI-2
Feature: The action bar names what the keys do to the selected stem
  In Table, Graph and Detail a one-line bar sits above the status bar,
  contextual to the selected stem: `■ x stop  ↻ r restart` while it runs,
  `▶ s start` when it is stopped or failed, then `: scripts (N)` (its
  custom scripts), `v variant …` (only with variants), `p watch …` (only
  with watch rules), `o editor` and `? more`. process-chain is declared
  a, b, c, d: `G` selects d, the leaf (no dependants).

  Scenario: a healthy stem shows x stop; after x it shows s start
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems attach --view table --headless --script 'wait:healthy;G;frame;x;wait:stem=d:stopped;frame'"
    Then the command succeeds
    And frame 1 contains "■ x stop  ↻ r restart  : scripts (0)  o editor  ? more"
    And frame 1 does not contain "▶ s start"
    And frame 2 contains "▶ s start  : scripts (0)  o editor  ? more"
    And frame 2 does not contain "x stop"
    And frame 2 contains "✓ stopped d"
