@FR-UI-1
Feature: The dashboard follows state changes live
  The table is driven by the daemon's event stream: a stem that crashes
  turns into `✗ failed` on its row without a restart of the dashboard.

  Scenario: a crashed stem shows ✗ failed on its row
    # restart.policy never: with the default on-failure (22) a crash goes
    # straight back to starting and the row never shows `failed`.
    Given the "minimal" workspace with a local override setting stems.echo-svc.restart.policy=never
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems attach --headless --script 'wait:healthy;frame;wait:stem=echo-svc:failed;frame'" in the background
    Then within 10s the background command's stdout contains "--- frame 1 ---"
    When the chaos endpoint "crash" is called on "echo-svc"
    Then within 10s the background command exits with code 0
    And within 1s the background command's stdout contains "✗ failed"
    When I run "stems down --all --json"
    Then the command succeeds
