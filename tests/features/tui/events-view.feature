@FR-UI-1
Feature: The Events view and the jump to a stem's logs
  The Events view lists the daemon's events (TIME KIND STEM FROM→TO REASON
  ACTOR), buffered ones included; `/` filters; `Enter` opens the event's
  stem in the Logs view, replayed from a minute before the event and
  positioned at the event time.

  Scenario: a crash shows healthy→failed; Enter shows the crash line
    # restart.policy never: the crash leaves the stem failed (see live-update).
    Given the "minimal" workspace with a local override setting stems.echo-svc.restart.policy=never
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems attach --headless --size 120x30 --script 'wait:healthy;chaos:crash;wait:stem=echo-svc:failed;view:events;/failed<Enter>;frame;Enter;wait:log=chaos crash requested;frame'"
    Then the command succeeds
    And frame 1 contains "[Events]"
    And frame 1 contains "TIME     KIND"
    And frame 1 contains "stem.state"
    And frame 1 contains "healthy→failed"
    And frame 2 contains "[Logs]"
    And frame 2 contains "Logs: echo-svc · scrolled"
    And frame 2 contains "ERROR chaos crash requested code=1"
    When I run "stems down --all --json"
    Then the command succeeds
