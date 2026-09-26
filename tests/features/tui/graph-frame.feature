@FR-UI-1 @FR-GR-4
Feature: The dashboard's graph view, as a text frame
  With more than one stem the dashboard opens on the dependency graph: the
  layout of `stems graph` with live glyphs, the selected box in reverse
  video and the glyph legend on the last line of the view. When the full
  boxes do not fit, or after `-`, boxes are compact (`[name ✓]`).

  Scenario: process-chain at 120x40 matches the golden
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems attach --headless --size 120x40 --script 'view:graph;wait:healthy;frame'"
    Then the command succeeds
    And the frame matches golden "tui-graph-process-chain"

  Scenario: compact boxes at 80x24 match the golden
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems attach --headless --size 80x24 --script 'view:graph;wait:healthy;-;frame'"
    Then the command succeeds
    And the frame matches golden "tui-graph-process-chain-compact"

  Scenario: the graph is the default view for more than one stem
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems attach --headless --script 'wait:healthy;frame'"
    Then the command succeeds
    And the last frame contains "[Graph]"
    And the last frame contains "✓ healthy  ! degraded  ✗ failed"
