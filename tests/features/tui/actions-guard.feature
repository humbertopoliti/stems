@FR-UI-2
Feature: Stopping a stem with running dependants asks first
  `x` on a stem that running stems depend on gets `HAS_DEPENDANTS` from
  the daemon (nothing is stopped) and opens a dialog naming them: `n`
  cancels, `y`/`X` stop them too. `X` on the table cascades at once.
  In process-chain, d depends on b (`j` selects b).

  Background:
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds

  Scenario: x names the running dependant
    When I run "stems attach --view table --headless --script 'wait:healthy;j;x;frame'"
    Then the command succeeds
    And the frame matches golden "tui-stop-confirm" masking PID,UPTIME,CPU,MEM
    And the last frame contains "Stop b? Running stems depend on it:"

  Scenario: n cancels, nothing stops
    When I run "stems attach --view table --headless --script 'wait:healthy;j;x;n;frame'"
    Then the command succeeds
    And the last frame does not contain "Running stems depend on it"
    And during 1s the events stream never contains {"kind": "stem.state", "stem": "b", "to": "stopping"}
    And within 1s the stem "b" is "healthy"
    And within 1s the stem "d" is "healthy"

  Scenario: X cascades: the dependant stops too
    When I run "stems attach --view table --headless --script 'wait:healthy;j;X;wait:stem=b:stopped;frame'"
    Then the command succeeds
    And within 3s the stem "b" is "stopped"
    And within 3s the stem "d" is "stopped"
    And within 1s the stem "a" is "healthy"
    And within 1s the stem "c" is "healthy"
