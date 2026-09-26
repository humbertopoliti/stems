@FR-UI-2 @FR-AI-4
Feature: Action keys on the selected stem
  In Table, Graph and Detail, `s` starts, `x` stops and `r` restarts the
  selected stem. The dashboard's requests carry `actor: tui:<user>`, so
  the events they cause are attributed to the TUI (FR-AI-4). Headless
  runs execute each action synchronously, then show its result toast.
  process-chain is declared a, b, c, d: `G` selects d, the leaf.

  Background:
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds

  Scenario: x stops the leaf stem, attributed to the TUI
    When I run "stems attach --view table --headless --script 'wait:healthy;G;frame;x;wait:stem=d:stopped;frame'"
    Then the command succeeds
    And frame 1 contains "› d"
    And the last frame contains "✓ stopped d"
    And within 3s the stem "d" is "stopped"
    And within 2s the stem "c" is "healthy"
    When I run "stems events --json --since 0"
    Then the JSON at "$[?@.kind=='stem.state' && @.stem=='d' && @.to=='stopping'].actor" contains "tui:"

  Scenario: s starts it again, r restarts it with a new pid
    When I run "stems stop d --json"
    Then the command succeeds
    When I run "stems attach --view table --headless --script 'wait:stem=d:stopped;G;s;wait:stem=d:healthy;frame'"
    Then the command succeeds
    And the last frame contains "✓ started d"
    And within 10s the stem "d" is "healthy"
    When I run "stems status --json"
    And I save the JSON at "$.data.stems[?@.name=='d'].pid" as "pid"
    When I run "stems attach --view table --headless --script 'wait:healthy;G;r;wait:stem=d:healthy;frame'"
    Then the command succeeds
    And the last frame contains "✓ restarted d"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='d'].pid" does not equal ${var:pid}
    When I run "stems events --json --since 0"
    Then the JSON at "$[?@.kind=='stem.state' && @.stem=='d' && @.to=='starting'].actor" contains "tui:"
