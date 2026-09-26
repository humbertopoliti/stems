@FR-UI-1
Feature: The dashboard table, as a text frame
  `stems attach --headless --script ...` replays keys against the daemon and
  prints each `frame` as text (deliverable 27), so the dashboard is
  testable without a terminal. The table has the `status` columns plus CPU
  and MEM sparklines; pids, uptimes and metrics are masked in the golden.

  Scenario: the minimal workspace renders one healthy row
    Given the "minimal" workspace is up
    When I run "stems attach --headless --script 'wait:healthy;frame'"
    Then the command succeeds
    And stdout contains "--- frame 1 ---"
    And the frame matches golden "tui-table-minimal" masking PID,UPTIME,CPU,MEM

  Scenario: frames are also written to a directory, and --json lists them
    Given the "minimal" workspace is up
    When I run "stems attach --json --headless --size 100x30 --frames-out ${tmp}/frames --script 'wait:healthy;frame;Tab;frame'"
    Then the command succeeds
    And the JSON at "$.data.detached" equals true
    And the JSON at "$.data.frames[1]" contains "[Detail]"
    And the file "${tmp}/frames/frame-002.txt" contains "Detail: echo-svc"
