@FR-UI-3 @FR-UI-1
Feature: Number keys and header tabs jump straight to a view
  `1` Graph, `2` Table, `3` Detail, `4` Logs, `5` Events, `6` Scripts, in
  every view, unless a text field or a modal has the keys: a digit typed
  into the table filter stays in the filter. The header numbers the tabs
  (`1 Graph  2 [Table]  3 Detail  4 Logs  5 Events  6 Scripts`); with
  `ui.mouse` a click on a tab switches to it (reducer tests: no headless
  mouse token).

  Background:
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds

  Scenario: 4 shows the Logs view, 1 the Graph, 3 the Detail, 5 the Events, 6 the Scripts
    When I run "stems attach --view table --headless --script 'wait:healthy;4;frame;1;frame;3;frame;5;frame;6;frame'"
    Then the command succeeds
    And frame 1 contains "4 [Logs]"
    And frame 1 contains "Logs: a"
    And frame 2 contains "1 [Graph]"
    And frame 2 does not contain "[Logs]"
    And frame 3 contains "3 [Detail]"
    And frame 3 contains "Detail: "
    And frame 4 contains "5 [Events]"
    And frame 5 contains "6 [Scripts]"

  Scenario: a digit typed into the filter does not switch views
    When I run "stems attach --view table --headless --script 'wait:healthy;/1<Enter>;frame;Esc;4;frame'"
    Then the command succeeds
    And frame 1 contains "2 [Table]"
    And frame 1 contains "/1"
    And frame 1 does not contain "[Logs]"
    And frame 1 does not contain "› a"
    And frame 2 contains "4 [Logs]"
