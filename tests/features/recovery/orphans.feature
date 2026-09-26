@FR-CR-4 @NFR-3 @recovery
Feature: Orphans on declared ports are found and only killed with consent
  `stems up` scans the workspace's declared ports first. A process that
  looks like the stem's own start command (`python3 app.py`) can be killed
  with `--yes`; anything else is only killed with `--kill-foreign`.

  @error
  Scenario: a stem started by hand is reported by up and killed by doctor --orphans --yes
    Given the "minimal" workspace
    And a stray process "cd ${repo}/examples/repos/shop-api && PORT=${port:18090} exec python3 app.py" is running in a new process group
    And within 10s port ${port:18090} is listening
    When I run "stems up --detach --json"
    Then the exit code is 3
    And the JSON error has code "ORPHANS_FOUND"
    And the JSON at "$.errors[0].details.orphans[0].port" equals ${port:18090}
    And the JSON at "$.errors[0].details.orphans[0].stem" equals "echo-svc"
    And the JSON at "$.errors[0].details.orphans[0].kind" equals "process"
    And the JSON at "$.errors[0].details.orphans[0].matches_start_command" equals true
    And the stray processes are still running
    And within 5s the lock file and socket do not exist
    When I run "stems doctor --orphans --json"
    Then the exit code is 3
    And the JSON at "$.data.orphans[0].action" equals "ignored"
    And the stray processes are still running
    When I run "stems doctor --orphans --yes --json"
    Then the command succeeds
    And the JSON at "$.data.orphans[0].action" equals "killed"
    And the JSON at "$.data.orphans[0].matches_start_command" equals true
    And the JSON at "$.data.remaining" equals 0
    When the stray processes are stopped
    And I run "stems doctor --orphans --json"
    Then the command succeeds
    And the JSON at "$.data.orphans" equals []
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  @error
  Scenario: a foreign process on a declared port survives --yes
    Given the "minimal" workspace
    And a stray process "cd ${tmp} && exec python3 -m http.server ${port:18090} --bind 127.0.0.1" is running in a new process group
    And within 10s port ${port:18090} is listening
    When I run "stems up --detach --json"
    Then the exit code is 3
    And the JSON error has code "ORPHANS_FOUND"
    And the JSON at "$.errors[0].details.orphans[0].matches_start_command" equals false
    When I run "stems doctor --orphans --yes --json"
    Then the exit code is 3
    And the JSON error has code "ORPHANS_FOUND"
    And the JSON at "$.data.orphans[0].action" equals "ignored"
    And the JSON at "$.data.orphans[0].matches_start_command" equals false
    And the JSON at "$.data.remaining" equals 1
    And the stray processes are still running
    When I run "stems up --detach --yes --json"
    Then the exit code is 1
    And the JSON error has code "PORT_IN_USE"
    And the stray processes are still running
    When I run "stems down --json"
    Then the command succeeds
    And within 5s the lock file and socket do not exist
    When the stray processes are stopped
    Then no process from the workspace's process groups is alive
