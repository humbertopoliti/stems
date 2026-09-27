@FR-UI-3
Feature: Keyboard navigation in the dashboard
  j/k (or arrows) move the selection, Enter opens the detail view, Tab
  cycles the views (the current one is shown in brackets in the title
  bar), / filters the table by name. process-chain has four stems, so the
  dashboard opens on the graph (28): the table scenarios ask for
  `--view table`.

  Background:
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds

  Scenario: j then Enter shows the second stem's detail
    # The config is the last section: a tall frame shows all of it.
    When I run "stems attach --view table --size 100x100 --headless --script 'wait:healthy;j;Enter;frame'"
    Then the command succeeds
    And the last frame contains "Detail: b"
    And the last frame contains "[Detail]"
    And the last frame contains "- stem: a"

  Scenario: Tab cycles the views
    When I run "stems attach --headless --script 'wait:healthy;frame;Tab;frame;Tab;frame;Tab;Tab;Tab;frame'"
    Then the command succeeds
    And frame 1 contains "[Graph]"
    And frame 2 contains "[Table]"
    And frame 3 contains "[Detail]"
    And frame 4 contains "[Graph]"

  Scenario: / filters the table
    When I run "stems attach --view table --headless --script 'wait:healthy;/b<Enter>;frame'"
    Then the command succeeds
    And the last frame contains "/b"
    And the last frame contains "› b"
    And the last frame does not contain "› a"
    And the last frame contains "18302"
    And the last frame does not contain "18301"

  Scenario: ? shows the help overlay and Esc closes it
    When I run "stems attach --headless --script 'wait:healthy;?;frame;Esc;frame'"
    Then the command succeeds
    And frame 1 contains "Help"
    And frame 2 does not contain "toggle this help"
