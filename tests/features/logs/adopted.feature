@FR-CR-2 @recovery
Feature: Logs of adopted stems
  After the daemon is killed (-9) and `stems up` adopts the still-running
  stems (deliverable 11), live output of the adopted processes is gone
  (their pipes died with the old daemon), but `stems logs` still returns
  the pre-crash lines from the files and notes the adoption.

  Scenario: pre-crash lines survive a daemon crash
    Given the "minimal" workspace is up
    When the chaos endpoint "logs?n=3&level=error" is called on "echo-svc"
    Then within 2s the stem log file "echo-svc/current.log" contains "ERROR chaos log line 2"
    When the daemon is killed with SIGKILL
    And I run "stems up --detach --json"
    Then the command succeeds
    And within 5s the events stream contains {"kind": "stem.adopted", "stem": "echo-svc"}
    When I run "stems logs echo-svc --json --grep 'chaos log line|adopted'"
    Then the command succeeds
    And the JSON nodes at "$[*].text" equal ["ERROR chaos log line 0", "ERROR chaos log line 1", "ERROR chaos log line 2", "[stems] adopted: live output unavailable, showing file log"]
    When I run "stems down --json"
    Then the command succeeds
