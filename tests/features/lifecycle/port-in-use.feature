@FR-ST-5 @error
Feature: A declared port held by another process
  Before starting a stem, stems checks that its declared ports are free. A
  port held by a foreign process fails the stem with PORT_IN_USE naming
  that process; stems never touches the foreign process. (Since 11, `up`
  first reports such a process as an orphan and starts nothing; `--yes`
  leaves foreign orphans alone and goes on to the port check.)

  Scenario: a stray http.server on echo-svc's port
    Given the "minimal" workspace
    And a stray process "exec python3 -m http.server ${port:18090} --bind 127.0.0.1" is running in a new process group
    And within 10s port ${port:18090} is listening
    When I run "stems up --detach --json"
    Then the exit code is 3
    And the JSON error has code "ORPHANS_FOUND"
    When I run "stems up --detach --yes --json"
    Then the exit code is 1
    And the JSON error has code "PORT_IN_USE"
    And the JSON at "$.data.failed[0].stem" equals "echo-svc"
    And the JSON at "$.errors[0].details.port" equals ${port:18090}
    And the JSON at "$.errors[0].details.pid" is greater than 1
    And the JSON at "$.errors[0].details.command" contains "ython"
    And the error message contains "already in use by pid"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].state" equals "failed"
    And the JSON at "$.data.stems[0].error.code" equals "PORT_IN_USE"
    When I run "stems down --all --json"
    Then the command succeeds
    And the stray processes are still running
    When the stray processes are stopped
