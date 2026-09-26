@FR-UI-1 @FR-UI-4
Feature: The split layout: logs under the table
  `Ctrl-L` shows the selected stem's logs (the same pane as the Logs view)
  in the bottom 40 % of Table/Graph/Detail, following. The choice is saved
  as `split_logs` in `ui.toml`, so the next attach starts split.

  Scenario: Ctrl-L on the table, then a second attach starts split
    Given the "minimal" workspace is up
    And the file "ui.toml" is written with "# mine"
    When I run "stems attach --view table --headless --script 'wait:healthy;Ctrl-L;wait:log=chaos endpoints enabled;frame'" with env STEMS_UI_CONFIG=${ws}/ui.toml
    Then the command succeeds
    And the frame matches golden "tui-split-table" masking PID,UPTIME,CPU,MEM,TIME
    And the file "${ws}/ui.toml" contains "split_logs = true"
    And the file "${ws}/ui.toml" contains "# mine"
    When I run "stems attach --view table --headless --script 'wait:healthy;wait:log=chaos endpoints enabled;frame;Ctrl-L;frame'" with env STEMS_UI_CONFIG=${ws}/ui.toml
    Then the command succeeds
    And frame 1 contains "Logs: echo-svc · following"
    And frame 1 contains "WARN chaos endpoints enabled under /__chaos/"
    And frame 2 does not contain "Logs: echo-svc"
    And the file "${ws}/ui.toml" contains "split_logs = false"
