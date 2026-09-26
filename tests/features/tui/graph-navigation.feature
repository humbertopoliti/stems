@FR-UI-3
Feature: Keyboard navigation in the graph view
  The graph starts on its first box (top of the leftmost column). `h`/`l`
  move across columns (preferring connected stems), `j`/`k` within one,
  `Enter` opens the selected stem's detail and `Esc` comes back.

  Background:
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds

  Scenario: l, j, Enter opens the detail of c
    When I run "stems attach --headless --script 'view:graph;wait:healthy;l;j;Enter;frame;Esc;frame'"
    Then the command succeeds
    And frame 1 contains "Detail: c"
    And frame 1 contains "[Detail]"
    And frame 2 contains "[Graph]"

  Scenario: arrows move like h/l/j/k
    When I run "stems attach --headless --script 'view:graph;wait:healthy;Right;Right;Left;Enter;frame'"
    Then the command succeeds
    And the last frame contains "Detail: b"
